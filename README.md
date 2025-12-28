# 查看 Rust 编译器不同阶段的输出

这个项目用于演示 Rust 编译器在不同阶段的代码表示。

## 使用方法

### 1. 查看宏展开后的代码（expanded）
```bash
cargo +nightly rustc -- -Zunpretty=expanded
```

### 2. 查看 HIR（High-level Intermediate Representation）
```bash
cargo +nightly rustc -- -Zunpretty=hir
```

或者查看树形结构：
```bash
cargo +nightly rustc -- -Zunpretty=hir-tree
```

### 3. 查看 MIR（Mid-level Intermediate Representation）
```bash
cargo +nightly rustc -- -Zunpretty=mir
```

或者输出到文件：
```bash
cargo +nightly rustc -- --emit mir
```

## 实际输出分析

### 1. Expanded（宏展开后）

**关键特征**：
- `async fn` 和 `.await` **仍然是原始语法**，没有被转换
- 只展开了宏（如 `println!` 被展开为 `::std::io::_print(format_args!(...))`）
- 添加了标准库的预导入（`#[prelude_import]`）

**示例输出片段**：
```rust
async fn foo() -> i32 {
    let x = 1;
    let y = x + 2;
    futures::future::ready(42).await;  // .await 仍然是原始语法
    let result = y + 10;
    result
}
```

**说明**：在 expanded 阶段，`async/await` 还没有被去糖，编译器只是展开了宏。

---

### 2. HIR（高级中间表示）

**关键特征**：
- `async fn` 被转换为 **coroutine 闭包**，接收 `mut _task_context: ResumeTy` 参数
- `.await` 被转换为 `match into_future(...) { mut __awaitee => loop { ... } }` 模式
- 可以看到 `yield ()` 和 `_task_context = (yield ())` 的协程机制
- 返回类型变为 `/*impl Trait*/ |mut _task_context: ResumeTy| { ... }`

**示例输出片段**：
```rust
async fn foo()
    ->
        /*impl Trait*/ |mut _task_context: ResumeTy|
    {
        match into_future(futures::future::ready(42)) {
            mut __awaitee =>
                loop {
                    match unsafe {
                            poll(new_unchecked(&mut __awaitee),
                                get_context(_task_context))
                        } {
                        Ready {  0: result } => break result,
                        Pending {} => { }
                    }
                    _task_context = (yield ());  // 协程暂停点
                },
        };
        // ... 后续代码
    }
```

**关键观察点**：
1. `async fn` 变成了一个闭包，接收 `ResumeTy` 参数
2. `.await` 变成了 `loop { match poll(...) { Ready => break, Pending => yield } }`
3. `yield ()` 表示协程的暂停点
4. `_task_context` 在每次 `yield` 时被更新

**这些函数/操作符的提供者**：
- **`into_future()`**：由 `IntoFuture` trait 提供
  - 定义：`library/core/src/future/into_future.rs`（第 133 行）
  - Lang item：`#[lang = "into_future"]`
  - 作用：将任何实现了 `IntoFuture` 的类型转换为 `Future`
  - 对于 `T: Future`，`IntoFuture` 有默认实现，直接返回 `self`
  
- **`poll()`**：由 `Future` trait 提供
  - 定义：`library/core/src/future/future.rs`（第 111-113 行）
  - Lang item：`#[lang = "poll"]`
  - 作用：轮询 Future，返回 `Poll::Ready(T)` 或 `Poll::Pending`
  
- **`yield`**：Rust 语言内置的协程关键字
  - 不是函数调用，是语言原语
  - 在 HIR 中表示为 `hir::ExprKind::Yield`
  - 作用：暂停协程执行，返回 `CoroutineState::Yielded`
  - 编译器在 MIR 阶段将其转换为状态设置和 `Poll::Pending` 返回

**说明**：HIR 是编译器的内部数据结构（不是 Rust 代码），这里显示的是格式化后的文本表示。

---

### 3. MIR（中级中间表示）

**关键特征**：
- 生成了 `foo::{closure#0}` 函数，这是 `Future::poll` 的实现
- 使用**状态机**，通过 `discriminant` 和 `switchInt` 来切换状态
- 代码被分解为多个**基本块**（basic blocks，如 `bb0`, `bb1`, `bb2`, ...）
- 状态保存在枚举中，有不同的 variant（0=未开始, 1=完成, 2=已销毁, 3=暂停点）
- 跨 `.await` 的局部变量（如 `y`）被提升到状态机结构体中

**示例输出片段**：
```rust
fn foo::{closure#0}(_1: Pin<&mut {async fn body of foo()}>, _2: &mut Context<'_>) -> Poll<i32> {
    // ...
    bb0: {
        _17 = discriminant((*_18));  // 获取当前状态
        switchInt(move _17) -> [0: bb1, 1: bb15, 2: bb14, 3: bb13, otherwise: bb8];
    }

    bb1: {
        // 状态 0：初始执行
        _3 = const 1_i32;
        // ... 计算 y = x + 2
        (((*_18) as variant#3).0: i32) = move (_4.0: i32);  // 保存 y 到状态机
        // ... 调用 poll
    }

    bb9: {
        // 如果 poll 返回 Pending
        _0 = Poll::<i32>::Pending;
        discriminant((*_18)) = 3;  // 设置状态为暂停点 3
        return;
    }

    bb13: {
        // 状态 3：从暂停点恢复
        goto -> bb5;  // 继续执行 poll
    }
}
```

**关键观察点**：
1. **状态机枚举**：通过 `discriminant` 和 `switchInt` 实现状态切换
2. **基本块结构**：代码被分解为基本块，每个块有明确的入口和出口
3. **变量提升**：跨 `.await` 的变量（如 `y`）被保存到状态机结构体中：`(((*_18) as variant#3).0: i32)`
4. **状态转换**：
   - `0` → 初始状态，执行代码
   - `1` → 完成状态
   - `2` → 已销毁状态（panic）
   - `3` → 暂停点，等待恢复
5. **Poll 返回**：`Poll::Pending` 时设置状态并返回，`Poll::Ready` 时设置完成状态并返回

**说明**：MIR 也是编译器的内部数据结构，这里显示的是格式化后的文本表示。MIR 是进行借用检查、优化和代码生成的基础。

#### MIR 中的 Coroutine 和 Future 关系

在 MIR 阶段，编译器将 HIR 中的 coroutine 转换为状态机，并生成 `Future::poll` 的实现。理解 coroutine 和 future 的关系对于理解 async/await 的底层机制至关重要。

**核心关系**：

1. **Coroutine 是底层机制**：提供"暂停/恢复"的通用能力
2. **Future 是上层应用**：利用 coroutine 实现"异步等待"的具体场景
3. **在 MIR 中**：async coroutine 类型同时实现了 `Coroutine` 和 `Future` 两个 trait

**状态机结构**：

在 MIR 中，编译器生成的状态机结构体布局如下：

```rust
// 编译器生成的状态机结构体（简化示意）
struct AsyncCoroutine {
    // 1. 捕获的变量（upvars，如果有闭包捕获）
    upvars: ...,
    
    // 2. 状态字段（discriminant）
    state: u32,  // 0 = 未开始, 1 = 完成, 2 = 已销毁, 3+ = 暂停点
    
    // 3. 跨暂停点存活的局部变量
    saved_locals: {
        y: i32,  // 例如：需要跨 .await 的变量
        // ... 其他变量
    },
}
```

**Future::poll 和 Coroutine::resume 的关系**：

编译器生成的 async coroutine 类型同时实现了 `Coroutine` 和 `Future` 两个 trait：

```rust
// 编译器自动为 async coroutine 实现 Future trait
impl Future for AsyncCoroutine {
    type Output = T;  // 原函数的返回类型
    
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        // 这个 poll 方法实际上是对 Coroutine::resume 的包装
        match self.resume(ResumeTy::from_context(cx)) {
            CoroutineState::Yielded(_) => Poll::Pending,
            CoroutineState::Complete(x) => Poll::Ready(x),
        }
    }
}

// 同时，这个类型也实现 Coroutine trait
impl Coroutine<ResumeTy> for AsyncCoroutine {
    type Yield = ();
    type Return = T;
    
    fn resume(self: Pin<&mut Self>, arg: ResumeTy) -> CoroutineState<(), T> {
        // 状态机逻辑：根据当前状态执行代码
        match self.state {
            0 => {
                // 初始状态：执行代码
                // ... 执行代码 ...
                if 遇到 yield {
                    self.state = 暂停点编号;
                    return CoroutineState::Yielded(());
                }
                self.state = 1;
                return CoroutineState::Complete(result);
            }
            n => {
                // 从暂停点 n 继续
                // ... 继续执行 ...
                if 再次遇到 yield {
                    self.state = 新的暂停点;
                    return CoroutineState::Yielded(());
                }
                self.state = 1;
                return CoroutineState::Complete(result);
            }
        }
    }
}
```

**在 MIR 输出中的体现**：

1. **函数签名**：`foo::{closure#0}` 函数的签名是 `Future::poll` 的签名
   ```rust
   fn foo::{closure#0}(
       _1: Pin<&mut {async fn body of foo()}>,  // self
       _2: &mut Context<'_>                      // cx
   ) -> Poll<i32>
   ```

2. **状态切换**：通过 `discriminant` 和 `switchInt` 实现状态机
   ```rust
   _17 = discriminant((*_18));  // 获取当前状态
   switchInt(move _17) -> [0: bb1, 1: bb15, 2: bb14, 3: bb13, ...];
   ```

3. **状态转换映射**：
   - `CoroutineState::Yielded(())` → `Poll::Pending`
   - `CoroutineState::Complete(x)` → `Poll::Ready(x)`

4. **变量保存**：跨暂停点的变量被保存到状态机结构体中
   ```rust
   // 保存变量到状态机
   (((*_18) as variant#3).0: i32) = move (_4.0: i32);
   
   // 从状态机恢复变量
   _4 = ((*_18) as variant#3).0: i32;
   ```

**关键转换点**：

在 MIR Transform 阶段（`compiler/rustc_mir_transform/src/coroutine.rs`），`StateTransform` pass 执行以下转换：

1. **yield 表达式转换**（对于 async）：
   ```rust
   // 转换前：yield ()
   yield ();
   
   // 转换后：设置状态 + 返回 Poll::Pending
   self.state = SuspensionPoint1;
   return Poll::Pending;
   ```

2. **return 表达式转换**（对于 async）：
   ```rust
   // 转换前：return x
   return x;
   
   // 转换后：设置完成状态 + 返回 Poll::Ready
   self.state = Done;
   return Poll::Ready(x);
   ```

3. **局部变量访问转换**：
   ```rust
   // 转换前：访问局部变量
   _1 = 42;
   
   // 转换后：访问状态机字段
   self.field_0 = 42;  // field_0 对应原来的 _1
   ```

**总结**：

- **Coroutine** 提供了通用的"暂停/恢复"机制，是 `async fn`、`gen fn` 和 `async gen fn` 的底层抽象
- **Future** 是 coroutine 在异步场景下的具体应用，`Future::poll` 是对 `Coroutine::resume` 的包装
- 在 MIR 中，状态机同时实现了 `Coroutine` 和 `Future` 两个 trait，通过状态切换实现异步执行
- 状态机结构体保存了跨暂停点的局部变量，使得协程可以在暂停后恢复执行

---

## 转换流程总结

```
源代码 (async fn + .await)
    ↓ [AST Lowering]
HIR (coroutine 闭包 + loop/match/yield)
    ↓ [MIR Transform]
MIR (状态机 + 基本块 + Poll::Pending/Ready)
    ↓ [代码生成]
LLVM IR → 机器码
```

**关键转换点**：
1. **HIR 阶段**：`async fn` → coroutine 闭包，`.await` → `loop { match poll(...) { Ready => break, Pending => yield } }`
2. **MIR 阶段**：coroutine → 状态机，`yield` → 状态设置 + `Poll::Pending`，`return` → 状态设置 + `Poll::Ready`

## 关键观察点

### 在 HIR 输出中寻找：
- ✅ `|mut _task_context: ResumeTy|` - coroutine 闭包的签名
- ✅ `match into_future(...)` - `.await` 的转换起点
- ✅ `loop { match poll(...) { Ready => break, Pending => { } } }` - `.await` 的核心逻辑
- ✅ `_task_context = (yield ())` - 协程暂停点，保存上下文

### 在 MIR 输出中寻找：
- ✅ `discriminant((*_18))` - 获取状态机的当前状态（coroutine 状态）
- ✅ `switchInt(...) -> [0: bb1, 1: bb15, 2: bb14, 3: bb13, ...]` - 状态切换（coroutine resume 逻辑）
- ✅ `(((*_18) as variant#3).0: i32)` - 访问状态机中保存的变量（跨暂停点的局部变量）
- ✅ `discriminant((*_18)) = 3` - 设置状态为暂停点（yield 的转换）
- ✅ `Poll::<i32>::Pending` / `Poll::<i32>::Ready(...)` - Future 的返回（CoroutineState 到 Poll 的转换）
- ✅ `bb0`, `bb1`, `bb2`, ... - 基本块（basic blocks）
- ✅ `fn foo::{closure#0}(_1: Pin<&mut ...>, _2: &mut Context<'_>) -> Poll<i32>` - Future::poll 的实现
- ✅ `{async fn body of foo()}` - 状态机结构体类型（coroutine 类型）

### 状态机状态说明（MIR 中）：
- **状态 0**：未开始，执行初始代码
- **状态 1**：已完成，如果再次 poll 会 panic
- **状态 2**：已销毁（panic 后），如果再次 poll 会 panic
- **状态 3+**：暂停点，从该点恢复执行

## 注意事项

- 需要使用 nightly 工具链（`+nightly`）
- HIR 和 MIR 的输出是格式化后的文本表示，实际的 HIR 和 MIR 是内存中的数据结构
- 输出可能很长，建议重定向到文件查看：
  ```bash
  cargo +nightly rustc -- -Zunpretty=hir > hir_output.txt
  cargo +nightly rustc -- -Zunpretty=mir > mir_output.txt
  ```
- 如果输出太长，可以使用 `less` 或编辑器查看：
  ```bash
  cargo +nightly rustc -- -Zunpretty=mir | less
  ```

## 相关资源

- [Rust 编译器开发指南 - HIR](https://rustc-dev-guide.rust-lang.org/hir.html)
- [Rust 编译器开发指南 - MIR](https://rustc-dev-guide.rust-lang.org/mir/index.html)
- [Rust Playground](https://play.rust-lang.org/) - 可以在线查看 MIR 输出

