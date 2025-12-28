# 查看 Rust 编译器不同阶段的输出

这个项目用于演示 Rust 编译器在不同阶段的代码表示。

**重要说明**：本文档中的 HIR 和 MIR 输出都是**格式化后的文本表示**，用于调试和查看。实际的 HIR 和 MIR 是编译器的**内部数据结构**（Rust 结构体和枚举的集合），存储在内存中，不是 Rust 代码。详见 `rust-async.md` 中的"补充：MIR 和 HIR 的设计与关系"部分。

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

**重要说明：`/*impl Trait*/ |mut _task_context: ResumeTy|` 既是 coroutine 也是 Future**

在 HIR 输出中，`/*impl Trait*/ |mut _task_context: ResumeTy|` 这个表示：

1. **从实现机制角度**：这是一个 **coroutine 闭包表达式**
    - 接收 `ResumeTy` 参数（coroutine 的特征）
    - 包含 `yield ()` 暂停点（coroutine 的核心机制）
    - 在 HIR 中表示为 `hir::ExprKind::Closure`，其 `coroutine_kind` 为 `CoroutineKind::Desugared(CoroutineDesugaring::Async, ...)`

2. **从类型系统角度**：返回类型 `/*impl Trait*/` 是一个实现了 **`Future` trait** 的 opaque type
    - 在类型检查阶段，编译器会为这个 opaque type 添加 `Future<Output = T>` bound
    - 源代码位置：`compiler/rustc_ast_lowering/src/item.rs`（第 1830-1872 行）
    - 对于 async coroutine，返回类型被转换为 `Future<Output = T>` bound（第 1851 行）

3. **两者的关系**：
    - **Coroutine 是底层实现机制**：提供了"暂停/恢复"的能力
    - **Future 是类型系统层面的抽象**：表示这是一个异步计算
    - 在 HIR 阶段，coroutine 闭包表达式会被类型系统识别为实现了 `Future` trait 的类型

**源代码证据**：

**文件**: `compiler/rustc_ast_lowering/src/item.rs`（第 1808 行）
```rust
hir::OpaqueTyOrigin::AsyncFn { parent: fn_def_id, in_trait_or_impl },
```
- `async fn` 的返回类型被标记为 `OpaqueTyOrigin::AsyncFn`

**文件**: `compiler/rustc_ast_lowering/src/item.rs`（第 1851 行）
```rust
CoroutineKind::Async { .. } => (sym::Output, hir::LangItem::Future),
```
- 对于 async coroutine，返回类型被转换为 `Future<Output = T>` bound

**总结**：在 HIR 阶段，`/*impl Trait*/ |mut _task_context: ResumeTy|` 既是 coroutine（实现机制），也是 Future（类型系统）。Coroutine 提供了底层的暂停/恢复能力，而 Future 是类型系统层面的抽象。

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
1. `async fn` 变成了一个 **coroutine 闭包**，接收 `ResumeTy` 参数（coroutine 的特征）
2. `.await` 变成了 `loop { match poll(...) { Ready => break, Pending => yield } }`
3. `yield ()` 表示协程的暂停点（coroutine 的核心机制）
4. `_task_context` 在每次 `yield` 时被更新
5. **返回类型 `/*impl Trait*/` 是一个实现了 `Future` trait 的 opaque type**
    - 在类型检查阶段，编译器会为这个 opaque type 添加 `Future<Output = T>` bound
    - 所以它既是 coroutine（实现机制），也是 Future（类型系统）

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

**重要说明**：HIR 是编译器的**内部数据结构**（不是 Rust 代码），这里显示的是格式化后的文本表示。虽然 HIR 保留了 Rust 的语法结构，但它已经是编译器的内存表示，不是源代码。详见 `rust-async.md` 中的"补充：MIR 和 HIR 的设计与关系"部分。

---

### 3. MIR（中级中间表示）

**关键特征**：
- 生成了 `foo::{closure#0}` 函数，这是 `Future::poll` 的实现
- 使用**状态机**，通过 `discriminant` 和 `switchInt` 来切换状态
- 代码被分解为多个**基本块**（basic blocks，如 `bb0`, `bb1`, `bb2`, ...）
- 状态保存在枚举中，有不同的 variant（0=Unresumed, 1=Returned, 2=Panicked, 3+=Suspend0/Suspend1/...）
- 跨 `.await` 的局部变量（如 `y`）被提升到状态机结构体中

**实际 MIR 输出分析**：

基于 `src/main.rs` 中的 `foo` 函数，实际的 MIR 输出如下：

```rust
// 1. foo 函数：返回状态机结构体
fn foo() -> {async fn body of foo()} {
    let mut _0: {async fn body of foo()};

    bb0: {
        _0 = {coroutine@src/main.rs:3:23: 12:2 (#0)};  // 创建 coroutine
        return;
    }
}

// 2. Future::poll 的实现
// 注意：在 MIR 中，只生成 Future::poll 的实现，没有独立的 Coroutine::resume 实现
// 状态机逻辑在 poll 内部实现，返回类型是 Poll<T> 而不是 CoroutineState
//
// **重要说明：Coroutine 在 MIR 层面的核心作用**
//
// 虽然只生成了 `Future::poll` 的实现，但 coroutine 在 MIR 层面仍然发挥着关键作用：
//
// 1. **状态机的生成**：整个状态机的结构、布局、状态切换逻辑都是基于 coroutine 机制生成的
//    - 状态字段（discriminant）的生成
//    - 状态变体（variant）的布局计算
//    - 暂停点（suspension points）的识别和状态分配
//
// 2. **变量提升机制**：基于 coroutine 的暂停/恢复机制，分析哪些变量需要跨暂停点保存
//    - `locals_live_across_suspend_points` 分析
//    - `compute_layout` 计算状态机布局
//    - 将局部变量映射到状态机结构体的字段
//
// 3. **统一的抽象**：coroutine 提供了统一的底层抽象，使得 `async fn`、`gen fn`、`async gen fn` 
//    都可以使用相同的机制
//    - 在 MIR Transform 阶段，根据 coroutine 类型选择生成 `Future::poll` 或 `Coroutine::resume`
//    - 对于 async coroutine，生成 `Future::poll`（返回 `Poll<T>`）
//    - 对于 gen coroutine，生成 `Coroutine::resume`（返回 `CoroutineState<Y, R>`）
//
// 4. **Drop 处理**：coroutine 的 drop shim 也是基于 coroutine 机制生成的
//    - 根据 coroutine 状态决定如何 drop
//    - 处理不同状态下的资源清理
//
// 5. **状态管理**：状态切换逻辑（discriminant、switchInt）都是 coroutine 机制的核心体现
//
// **总结**：虽然最终只生成了 `Future::poll` 的实现，但整个状态机的生成、变量提升、布局计算
// 等核心机制都是基于 coroutine 的。Coroutine 提供了"暂停/恢复"的底层抽象，而 `Future::poll`
// 只是这个抽象在异步场景下的具体应用。
fn foo::{closure#0}(
    _1: Pin<&mut {async fn body of foo()}>,  // self: 状态机结构体
    _2: &mut Context<'_>                      // cx: 异步上下文
) -> Poll<i32> {
    let mut _18: &mut {async fn body of foo()};
    let mut _17: u32;

    bb0: {
        _18 = copy (_1.0: &mut {async fn body of foo()});
        _17 = discriminant((*_18));  // 获取当前状态
        switchInt(move _17) -> [
            0: bb1,   // 状态 0：Unresumed（未开始）
            1: bb15,  // 状态 1：Returned（已完成）
            2: bb14,  // 状态 2：Panicked（已销毁）
            3: bb13,  // 状态 3：Suspend0（第一个暂停点）
            otherwise: bb8
        ];
    }

    // 状态 0：Unresumed（初始执行）
    bb1: {
        _3 = const 1_i32;  // x = 1
        _4 = AddWithOverflow(copy _3, const 2_i32);  // y = x + 2
        assert(!move (_4.1: bool), "...") -> [success: bb2, unwind: bb12];
    }

    bb2: {
        // 保存 y 到状态机的 variant#3（Suspend0）中
        // variant#3 包含两个字段：
        //   .0: i32 (y)
        //   .1: futures::future::Ready<i32> (__awaitee)
        (((*_18) as variant#3).0: i32) = move (_4.0: i32);
        _6 = futures::future::ready::<i32>(const 42_i32) -> [return: bb3, unwind: bb12];
    }

    bb3: {
        _5 = <Ready<i32> as IntoFuture>::into_future(move _6) -> [return: bb4, unwind: bb12];
    }

    bb4: {
        // 保存 __awaitee 到状态机
        (((*_18) as variant#3).1: futures::future::Ready<i32>) = move _5;
        goto -> bb5;
    }

    bb5: {
        _9 = &mut (((*_18) as variant#3).1: futures::future::Ready<i32>);
        _8 = Pin::<&mut Ready<i32>>::new_unchecked(copy _9) -> [return: bb6, unwind: bb12];
    }

    bb6: {
        _10 = copy _2;
        _7 = <Ready<i32> as Future>::poll(move _8, copy _10) -> [return: bb7, unwind: bb12];
    }

    bb7: {
        _11 = discriminant(_7);  // 检查 poll 的结果
        switchInt(move _11) -> [
            0: bb10,  // Poll::Ready
            1: bb9,   // Poll::Pending
            otherwise: bb8
        ];
    }

    // 如果 poll 返回 Pending（对应 yield）
    bb9: {
        _0 = Poll::<i32>::Pending;  // 返回 Poll::Pending
        discriminant((*_18)) = 3;     // 设置状态为暂停点 3
        return;
    }

    // 如果 poll 返回 Ready（继续执行）
    bb10: {
        _12 = copy ((_7 as Ready).0: i32);  // 获取 await 的结果
        _15 = copy (((*_18) as variant#3).0: i32);  // 从状态机恢复 y
        _16 = AddWithOverflow(copy _15, const 10_i32);  // result = y + 10
        assert(!move (_16.1: bool), "...") -> [success: bb11, unwind: bb12];
    }

    bb11: {
        _14 = move (_16.0: i32);
        _0 = Poll::<i32>::Ready(copy _14);  // 返回 Poll::Ready(result)
        discriminant((*_18)) = 1;            // 设置状态为完成
        return;
    }

    // 状态 3：Suspend0（从暂停点恢复）
    bb13: {
        _13 = move _2;
        _2 = move _13;
        goto -> bb5;  // 跳转到 bb5，继续执行 poll
    }

    // 状态 1：Returned（已完成）
    bb15: {
        assert(const false, "`async fn` resumed after completion");
    }

    // 状态 2：Panicked（已销毁）
    bb14: {
        assert(const false, "`async fn` resumed after panicking");
    }

    // cleanup 块：panic 时设置状态为 2
    bb12 (cleanup): {
        discriminant((*_18)) = 2;  // 设置状态为 Panicked
        resume;
    }
}
```

**关键观察点**：

1. **状态机结构体**：`{async fn body of foo()}` 是编译器生成的状态机类型
2. **variant#3 的布局**：包含两个字段
    - `.0: i32` - 跨暂停点存活的变量 `y`
    - `.1: futures::future::Ready<i32>` - 正在等待的 Future `__awaitee`
3. **状态切换**：在 `bb0` 中通过 `discriminant` 和 `switchInt` 实现状态机
4. **变量保存**：在 `bb2` 和 `bb4` 中保存变量到状态机
5. **变量恢复**：在 `bb10` 中从状态机恢复 `y` 的值
6. **yield 转换**：在 `bb9` 中，如果 poll 返回 `Pending`，设置状态为 3 并返回 `Poll::Pending`
7. **return 转换**：在 `bb11` 中，设置状态为 1 并返回 `Poll::Ready`

**关键观察点**：
1. **状态机枚举**：通过 `discriminant` 和 `switchInt` 实现状态切换
    - 在 `bb0` 中：`_17 = discriminant((*_18)); switchInt(move _17) -> [0: bb1, 1: bb15, 2: bb14, 3: bb13, ...]`

2. **基本块结构**：代码被分解为基本块，每个块有明确的入口和出口
    - `bb0`: 状态切换
    - `bb1-bb11`: 状态 0（初始执行）
    - `bb13`: 状态 3（从暂停点恢复）
    - `bb14-bb15`: 错误状态处理
    - `bb12`: cleanup 块（panic 处理）

3. **变量提升**：跨 `.await` 的变量被保存到状态机结构体中
    - `y` 保存在 `variant#3.0`：`(((*_18) as variant#3).0: i32)`
    - `__awaitee` 保存在 `variant#3.1`：`(((*_18) as variant#3).1: futures::future::Ready<i32>)`
    - 在 debug 信息中可以看到：`debug y => (((*_18) as variant#3).0: i32);`

4. **状态转换**：
    - `0` (bb1) → `Unresumed`（初始状态），执行代码
    - `1` (bb15) → `Returned`（完成状态），如果再次 poll 会 assert(false)
    - `2` (bb14) → `Panicked`（已销毁状态，panic 后），如果 poll 会 assert(false)
    - `3` (bb13) → `Suspend0`（第一个暂停点），从该点恢复执行

5. **Poll 返回**：
    - `Poll::Pending` (bb9)：设置状态为 3，返回 `Poll::Pending`
    - `Poll::Ready` (bb11)：设置状态为 1，返回 `Poll::Ready(result)`
    - Poll 结果的检查在 `bb7` 中：`_11 = discriminant(_7); switchInt(move _11) -> [0: bb10, 1: bb9, ...]`
    - **注意**：`Poll::Ready` 的 discriminant 是 0，`Poll::Pending` 的 discriminant 是 1

6. **执行流程**：
    - 首次 poll：bb0 → bb1 → ... → bb7 → (bb9 或 bb10 → bb11)
    - 从暂停点恢复：bb0 → bb13 → bb5 → ... → bb7 → (bb9 或 bb10 → bb11)

**重要说明**：MIR 也是编译器的**内部数据结构**（不是 Rust 代码），这里显示的是格式化后的文本表示。MIR 是基于控制流图（CFG）的数据结构，使用基本块和边表示程序，是进行借用检查、优化和代码生成的基础。详见 `rust-async.md` 中的"补充：MIR 和 HIR 的设计与关系"部分。

#### MIR 中的 Coroutine 和 Future 关系

在 MIR 阶段，编译器将 HIR 中的 coroutine 转换为状态机，并生成 `Future::poll` 的实现。理解 coroutine 和 future 的关系对于理解 async/await 的底层机制至关重要。

**核心关系**：

1. **Coroutine 是底层机制**：提供"暂停/恢复"的通用能力
2. **Future 是上层应用**：利用 coroutine 实现"异步等待"的具体场景
3. **在 MIR 中**：async coroutine 类型同时实现了 `Coroutine` 和 `Future` 两个 trait

**状态机结构**：

在 MIR 中，编译器生成的状态机结构体布局如下（基于实际输出）：

```rust
// 编译器生成的状态机结构体（实际 MIR 中的表示）
{async fn body of foo()} {
    // 1. 状态字段（discriminant）
    discriminant: u32,  // 0 = Unresumed, 1 = Returned, 2 = Panicked, 3+ = Suspend0/Suspend1/...
    
    // 2. 状态变体（variant）
    // 对于每个暂停点，编译器会生成一个 variant
    variant#3: Suspend0 {  // 状态 3：第一个暂停点
        // 2.1 跨暂停点存活的局部变量
        .0: i32,  // y - 需要跨 .await 使用的变量
        
        // 2.2 正在等待的 Future
        .1: futures::future::Ready<i32>,  // __awaitee - 正在等待的 Future
    },
    
    // 注意：如果函数没有捕获外部变量，则没有 upvars
    // 如果有闭包捕获，upvars 会在 discriminant 之前
}
```

**实际输出中的体现**：

在 MIR 输出中，可以看到：
- `debug y => (((*_18) as variant#3).0: i32)` - y 保存在 variant#3 的第 0 个字段
- `debug __awaitee => (((*_18) as variant#3).1: futures::future::Ready<i32>)` - __awaitee 保存在 variant#3 的第 1 个字段

**布局计算**：编译器通过 `compute_layout` 函数（`compiler/rustc_mir_transform/src/coroutine.rs` 第 1934 行）分析哪些变量需要跨暂停点保存，并生成相应的 variant 布局。

**Future::poll 和 Coroutine::resume 的关系**：

**重要澄清**：虽然在类型系统层面，async coroutine 类型同时实现了 `Coroutine` 和 `Future` 两个 trait，但在 **MIR 中，编译器只生成一个 `Future::poll` 的实现**，没有独立的 `Coroutine::resume` 实现。

**原因**：在 MIR Transform 阶段（`compiler/rustc_mir_transform/src/coroutine.rs` 第 1864-1870 行），编译器已经将 `CoroutineState` 转换为 `Poll`，所以只需要生成一个 `Future::poll` 实现，返回类型直接是 `Poll<T>` 而不是 `CoroutineState<(), T>`。

**概念上的对应关系**（用于理解，但实际 MIR 中不这样实现）：

```rust
// 概念上：Future::poll 内部包含状态机逻辑
impl Future for AsyncCoroutine {
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        // 状态机逻辑（在概念上对应 Coroutine::resume 的逻辑）
        match self.state {
            0 => {
                // 初始状态：执行代码
                // ... 执行代码 ...
                if 遇到 yield {
                    self.state = 暂停点编号;
                    return Poll::Pending;  // 直接返回 Poll::Pending，而不是 CoroutineState::Yielded
                }
                self.state = 1;
                return Poll::Ready(result);  // 直接返回 Poll::Ready，而不是 CoroutineState::Complete
            }
            n => {
                // 从暂停点 n 继续
                // ... 继续执行 ...
                if 再次遇到 yield {
                    self.state = 新的暂停点;
                    return Poll::Pending;
                }
                self.state = 1;
                return Poll::Ready(result);
            }
        }
    }
}
```

**实际 MIR 中的实现**：

在 MIR 中，我们只看到一个函数 `foo::{closure#0}`，它的签名是 `Future::poll` 的签名，返回 `Poll<i32>`。这个函数内部包含了完整的状态机逻辑，直接返回 `Poll::Pending` 或 `Poll::Ready`，而不是 `CoroutineState`。

**在 MIR 输出中的体现**：

1. **函数签名**：`foo::{closure#0}` 函数的签名是 `Future::poll` 的签名
   ```rust
   fn foo::{closure#0}(
       _1: Pin<&mut {async fn body of foo()}>,  // self: 状态机结构体
       _2: &mut Context<'_>                      // cx: 异步上下文
   ) -> Poll<i32>  // 返回类型：直接返回 Poll<T>，而不是 CoroutineState
   ```

   **关键点**：
    - **在 MIR 中，只生成 `Future::poll` 的实现**，没有独立的 `Coroutine::resume` 实现
    - 状态机逻辑在 `poll` 内部实现，直接返回 `Poll::Pending` 或 `Poll::Ready`
    - **对于 async coroutine，`Coroutine::resume` 方法在 MIR 中没有对应的代码**
    - 虽然在类型系统层面，async coroutine 类型同时实现了 `Coroutine` 和 `Future` 两个 trait，但在 MIR 代码生成阶段，编译器只生成 `Future::poll` 的实现
    - 这是因为在 MIR Transform 阶段（`compiler/rustc_mir_transform/src/coroutine.rs` 第 1864-1870 行），编译器已经将 `CoroutineState` 转换为 `Poll`，所以只需要一个 `Future::poll` 实现
    - **注意**：根据 `compiler/rustc_mir_transform/src/coroutine.rs` 第 1216 行的说明，编译器会根据 coroutine 类型选择生成 `Coroutine::resume` 或 `Future::poll`。对于 async coroutine，生成 `Future::poll`；对于其他类型的 coroutine（如 gen），可能生成 `Coroutine::resume`

2. **状态机结构体布局**：在 MIR 中，状态机结构体的布局如下：
   ```rust
   // 编译器生成的状态机结构体（在 MIR 中的表示）
   struct {async fn body of foo()} {
       // 1. 捕获的变量（upvars，如果有闭包捕获）
       // 在这个例子中没有，因为 foo 是普通函数
       
       // 2. 状态字段（discriminant）
       discriminant: u32,  // 0 = Unresumed, 1 = Returned, 2 = Panicked, 3+ = Suspend0/Suspend1/...
       
       // 3. 跨暂停点存活的局部变量（saved_locals）
       // 根据 COMPILE.md，只有跨 .await 的变量才会被提升
       // 在这个例子中，y 需要跨 .await，所以会被提升
       variant#3: Suspend0 {  // 状态 3：第一个暂停点（Suspend0）
           y: i32,  // 跨暂停点存活的变量
           __awaitee: Ready<i32>,  // 正在等待的 Future
       },
   }
   ```

   **布局计算**：编译器通过 `compute_layout` 函数（`compiler/rustc_mir_transform/src/coroutine.rs` 第 1934 行）计算状态机布局，分析哪些变量需要跨暂停点保存。

3. **状态切换**：通过 `discriminant` 和 `switchInt` 实现状态机（状态机逻辑在 `Future::poll` 内部实现）
   ```rust
   bb0: {
       // 获取当前状态（从状态机结构体中读取 discriminant）
       _17 = discriminant((*_18));  // _18 是 self 的引用
       
       // 根据状态跳转到不同的基本块
       // 这对应 Coroutine::resume 中的 match self.state { ... }
       switchInt(move _17) -> [
           0: bb1,   // 状态 0：Unresumed（未开始）
           1: bb15,  // 状态 1：Returned（已完成）
           2: bb14,  // 状态 2：Panicked（已销毁）
           3: bb13,  // 状态 3：Suspend0（第一个暂停点）
           otherwise: bb8  // 其他状态（不应该到达）
       ];
   }
   ```

4. **状态转换映射**：在 MIR Transform 阶段，编译器将 `CoroutineState` 转换为 `Poll`：
    - `CoroutineState::Yielded(())` → `Poll::Pending`
    - `CoroutineState::Complete(x)` → `Poll::Ready(x)`

   这个转换发生在 `compiler/rustc_mir_transform/src/coroutine.rs` 第 1864-1870 行：
   ```rust
   CoroutineKind::Desugared(CoroutineDesugaring::Async, _) => {
       // Compute Poll<return_ty>
       let poll_did = tcx.require_lang_item(LangItem::Poll, body.span);
       let poll_adt_ref = tcx.adt_def(poll_did);
       let poll_args = tcx.mk_args(&[old_ret_ty.into()]);
       Ty::new_adt(tcx, poll_adt_ref, poll_args)  // 返回 Poll<T> 类型
   }
   ```

5. **变量提升和访问**：跨暂停点的变量被提升到状态机结构体中
   ```rust
   // 在状态 0（Unresumed）中：计算 y 并保存到状态机
   bb1: {
       _3 = const 1_i32;  // x = 1
       _4 = AddWithOverflow(copy _3, const 2_i32);  // y = x + 2
       assert(!move (_4.1: bool), "...") -> [success: bb2, unwind: bb12];
   }
   
   bb2: {
       // 保存 y 到状态机的 variant#3.0（Suspend0 的第一个字段）
       // variant#3 包含两个字段：
       //   .0: i32 (y)
       //   .1: futures::future::Ready<i32> (__awaitee)
       (((*_18) as variant#3).0: i32) = move (_4.0: i32);
       
       // 创建 Future
       _6 = futures::future::ready::<i32>(const 42_i32) -> [return: bb3, unwind: bb12];
   }
   
   bb3: {
       _5 = <Ready<i32> as IntoFuture>::into_future(move _6) -> [return: bb4, unwind: bb12];
   }
   
   bb4: {
       // 保存 __awaitee 到状态机的 variant#3.1（Suspend0 的第二个字段）
       (((*_18) as variant#3).1: futures::future::Ready<i32>) = move _5;
       goto -> bb5;
   }
   
   // 在状态 3（Suspend0）中：从状态机恢复变量
   bb13: {
       // 从暂停点恢复，直接跳转到 bb5 继续执行 poll
       // y 的值在 bb10 中从状态机恢复
       goto -> bb5;
   }
   
   bb10: {
       // 从状态机中恢复 y 的值
       _15 = copy (((*_18) as variant#3).0: i32);  // 恢复 y
       
       // 继续使用 y 进行计算
       _16 = AddWithOverflow(copy _15, const 10_i32);  // result = y + 10
       assert(!move (_16.1: bool), "...") -> [success: bb11, unwind: bb12];
   }
   ```

   **变量提升规则**：只有跨暂停点存活的变量才会被提升。编译器通过 `locals_live_across_suspend_points` 函数（第 1918-1919 行）分析哪些变量需要提升。

   **注意**：`x` 不需要提升，因为它在 `.await` 之前就完成了使用。只有 `y` 需要跨 `.await` 使用，所以被提升到状态机中。

6. **yield 的转换**：在 MIR Transform 阶段，`yield` 被转换为状态设置和 `Poll::Pending` 返回
   ```rust
   // 转换前（HIR 阶段）：yield ()
   _task_context = (yield ());
   
   // 转换后（MIR 阶段）：
   bb7: {
       // 检查 poll 的结果
       _11 = discriminant(_7);  // 获取 Poll 的 discriminant
       switchInt(move _11) -> [
           0: bb10,  // Poll::Ready (discriminant = 0)
           1: bb9,   // Poll::Pending (discriminant = 1)
           otherwise: bb8
       ];
   }
   
   bb9: {
       // 如果 poll 返回 Pending，设置状态为暂停点 3
       _0 = Poll::<i32>::Pending;  // 返回 Poll::Pending
       discriminant((*_18)) = 3;   // 设置状态为 Suspend0
       return;
   }
   ```

   这个转换由 `TransformVisitor` 完成（第 1966-1978 行），当遇到 `TerminatorKind::Yield` 时，会：
    - 计算新的状态编号（第 465 行：`state = RESERVED_VARIANTS + suspension_points.len()`）
    - 设置 discriminant（第 393 行：`set_discr(state, source_info)`）
    - 将 `Yield` terminator 替换为 `Return`（第 396 行）

   **注意**：在实际输出中，`Poll::Ready` 的 discriminant 是 0，`Poll::Pending` 的 discriminant 是 1。

7. **return 的转换**：在 MIR Transform 阶段，`return` 被转换为状态设置和 `Poll::Ready` 返回
   ```rust
   // 转换前（HIR 阶段）：return result
   return result;
   
   // 转换后（MIR 阶段）：
   bb10: {
       // 如果 poll 返回 Ready，继续执行
       _12 = copy ((_7 as Ready).0: i32);  // 获取 await 的结果
       _15 = copy (((*_18) as variant#3).0: i32);  // 恢复 y
       _16 = AddWithOverflow(copy _15, const 10_i32);  // result = y + 10
       assert(!move (_16.1: bool), "...") -> [success: bb11, unwind: bb12];
   }
   
   bb11: {
       // 设置状态为完成（状态 1：Returned）
       _14 = move (_16.0: i32);
       _0 = Poll::<i32>::Ready(copy _14);  // 返回 Poll::Ready(result)
       discriminant((*_18)) = 1;          // 设置状态为 1（Returned）
       return;
   }
   ```

   **注意**：在实际输出中，return 的转换分为两个基本块：bb10 计算返回值，bb11 设置状态并返回。

8. **ResumeTy 的替换**：在 MIR Transform 阶段，`ResumeTy` 被替换为 `&mut Context<'_>`
   ```rust
   // 转换前（HIR 阶段）：使用 ResumeTy
   let cx = get_context(_task_context);  // _task_context: ResumeTy
   
   // 转换后（MIR 阶段）：直接使用 &mut Context<'_>
   // _2 参数直接是 &mut Context<'_>，不再需要转换
   ```

   这个转换由 `transform_async_context` 函数完成（第 1906 行），将 async 函数体中的所有 `ResumeTy` 替换为 `&mut Context<'_>`。

**完整的 MIR 代码执行流程**：

基于实际输出，以下是状态机的完整执行流程：

```
状态 0 (bb1-bb11): 初始执行
  ├─ bb1: 计算 y = x + 2
  ├─ bb2: 保存 y 到状态机 variant#3.0
  ├─ bb3: 调用 into_future
  ├─ bb4: 保存 __awaitee 到状态机 variant#3.1
  ├─ bb5: 创建 Pin<&mut Ready<i32>>
  ├─ bb6: 调用 Future::poll
  ├─ bb7: 检查 poll 结果
  │   ├─ Poll::Ready(0) → bb10 (继续执行)
  │   └─ Poll::Pending(1) → bb9 (暂停)
  ├─ bb10: 恢复 y，计算 result = y + 10
  └─ bb11: 返回 Poll::Ready(result)，设置状态为 1

状态 3 (bb13): 从暂停点恢复
  └─ bb13: 跳转到 bb5，继续执行 poll

状态 1 (bb15): 已完成
  └─ bb15: assert(false, "resumed after completion")

状态 2 (bb14): 已销毁
  └─ bb14: assert(false, "resumed after panicking")

cleanup (bb12): panic 处理
  └─ bb12: 设置状态为 2，resume
```

**关键执行路径**：

1. **首次 poll（状态 0）**：
    - 计算 `y = x + 2`（bb1）
    - 保存 `y` 到状态机（bb2）
    - 创建并保存 `__awaitee`（bb3-bb4）
    - 调用 `poll`（bb5-bb6）
    - 如果 `Pending`：设置状态为 3，返回 `Poll::Pending`（bb9）
    - 如果 `Ready`：恢复 `y`，计算 `result`，返回 `Poll::Ready`（bb10-bb11）

2. **从暂停点恢复（状态 3）**：
    - 跳转到 bb5，继续执行 `poll`（bb13）
    - 后续流程与首次 poll 相同

3. **完成后的处理**：
    - 状态 1：如果再次 poll，触发 assert（bb15）
    - 状态 2：如果 poll 已销毁的协程，触发 assert（bb14）

**关键转换点**：

在 MIR Transform 阶段（`compiler/rustc_mir_transform/src/coroutine.rs`），`StateTransform` pass 执行以下转换：

1. **yield 表达式转换**（对于 async）：
   ```rust
   // 转换前（HIR 阶段）：yield ()
   _task_context = (yield ());
   
   // 转换后（MIR 阶段）：
   // 1. 计算新的状态编号（第 465 行）
   let state = RESERVED_VARIANTS + suspension_points.len();  // 3 + 0 = 3
   
   // 2. 设置 discriminant（第 393 行）
   discriminant((*_18)) = 3;
   
   // 3. 返回 Poll::Pending（第 396 行）
   _0 = Poll::<i32>::Pending;
   return;
   ```

   转换代码位置：`compiler/rustc_mir_transform/src/coroutine.rs` 第 460-497 行

2. **return 表达式转换**（对于 async）：
   ```rust
   // 转换前（HIR 阶段）：return x
   return x;
   
   // 转换后（MIR 阶段）：
   // 1. 设置状态为完成（状态 1）
   discriminant((*_18)) = 1;
   
   // 2. 返回 Poll::Ready(x)
   _0 = Poll::<i32>::Ready(x);
   return;
   ```

3. **局部变量访问转换**：
   ```rust
   // 转换前：访问局部变量
   _1 = 42;
   
   // 转换后：访问状态机字段
   // 如果 _1 需要跨暂停点，会被映射到状态机字段
   self.field_0 = 42;  // field_0 对应原来的 _1
   ```

   转换由 `TransformVisitor` 完成（第 1966-1978 行），通过 `remap` 映射将局部变量访问转换为状态机字段访问。

4. **ResumeTy 替换**：
   ```rust
   // 转换前（HIR 阶段）：使用 ResumeTy
   let cx = get_context(_task_context);  // _task_context: ResumeTy
   
   // 转换后（MIR 阶段）：直接使用 &mut Context<'_>
   // _2 参数直接是 &mut Context<'_>，不再需要 get_context 调用
   ```

   转换由 `transform_async_context` 函数完成（第 1906 行），在 MIR Transform 阶段将所有 `ResumeTy` 替换为 `&mut Context<'_>`。

**如何在 MIR 输出中识别 Coroutine 和 Future**：

1. **识别状态机结构体类型**：
    - 查找 `{async fn body of foo()}` 这样的类型，这是编译器生成的状态机结构体
    - 在 `fn foo()` 中可以看到：`_0 = {coroutine@src/main.rs:3:23: 12:2 (#0)}`
    - 这个类型同时实现了 `Coroutine` 和 `Future` 两个 trait

2. **识别 Future::poll 实现**：
    - 查找 `foo::{closure#0}` 函数，这是 `Future::poll` 的实现
    - 函数签名：`fn(_1: Pin<&mut {async fn body of foo()}>, _2: &mut Context<'_>) -> Poll<i32>`
    - 这个函数内部实现了状态机逻辑，对应 `Coroutine::resume` 的实现
    - 在 debug 信息中可以看到：`debug _task_context => _2;`

3. **识别状态切换逻辑**：
    - 在 `bb0` 中查找 `discriminant((*_18))` 和 `switchInt`，这是状态机的核心
    - 模式：`switchInt(move _17) -> [0: bb1, 1: bb15, 2: bb14, 3: bb13, otherwise: bb8]`
    - 状态 0、1、2 是保留状态（Unresumed、Returned、Panicked）
    - 状态 3+ 是暂停点（Suspend0、Suspend1、...）

4. **识别变量提升**：
    - 查找 `(((*_18) as variant#3).0: i32)` 这样的模式，这是访问状态机中保存的变量
    - 在 debug 信息中可以看到：
        - `debug y => (((*_18) as variant#3).0: i32);` - y 保存在 variant#3.0
        - `debug __awaitee => (((*_18) as variant#3).1: futures::future::Ready<i32>);` - __awaitee 保存在 variant#3.1
    - 只有跨暂停点的变量才会被提升到状态机中

5. **识别 yield 转换**：
    - 查找 `bb7` 中的 `discriminant(_7)` 和 `switchInt`，这是检查 poll 结果
    - 查找 `bb9` 中的 `Poll::<i32>::Pending` 和 `discriminant((*_18)) = 3` 的组合
    - 模式：
      ```rust
      bb9: {
          _0 = Poll::<i32>::Pending;
          discriminant((*_18)) = 3;
          return;
      }
      ```
    - 对应 `CoroutineState::Yielded(())` → `Poll::Pending`

6. **识别 return 转换**：
    - 查找 `bb11` 中的 `Poll::<i32>::Ready(...)` 和 `discriminant((*_18)) = 1` 的组合
    - 模式：
      ```rust
      bb11: {
          _0 = Poll::<i32>::Ready(copy _14);
          discriminant((*_18)) = 1;
          return;
      }
      ```
    - 对应 `CoroutineState::Complete(x)` → `Poll::Ready(x)`

7. **识别从暂停点恢复**：
    - 查找 `bb13` 中的 `goto -> bb5`，这是从暂停点恢复的逻辑
    - 模式：
      ```rust
      bb13: {
          _13 = move _2;
          _2 = move _13;
          goto -> bb5;  // 跳转到 poll 调用处
      }
      ```

8. **识别 panic 处理**：
    - 查找 `bb12 (cleanup)` 中的 `discriminant((*_18)) = 2`，这是 panic 时的状态设置
    - 查找 `bb14` 和 `bb15` 中的 `assert(const false, "...")`，这是错误状态的处理

**总结**：

- **Coroutine 在 MIR 层面的核心作用**：
    - **状态机的生成**：整个状态机的结构、布局、状态切换逻辑都是基于 coroutine 机制生成的
    - **变量提升机制**：基于 coroutine 的暂停/恢复机制，分析哪些变量需要跨暂停点保存
    - **统一的抽象**：提供统一的底层抽象，使得 `async fn`、`gen fn`、`async gen fn` 都可以使用相同的机制
    - **Drop 处理**：coroutine 的 drop shim 也是基于 coroutine 机制生成的
    - **状态管理**：状态切换逻辑（discriminant、switchInt）都是 coroutine 机制的核心体现

- **Future 是 coroutine 在异步场景下的具体应用**：
    - 对于 async coroutine，编译器生成 `Future::poll` 实现（返回 `Poll<T>`）
    - 对于 gen coroutine，编译器生成 `Coroutine::resume` 实现（返回 `CoroutineState<Y, R>`）
    - 两者都基于相同的 coroutine 状态机机制

- **在 MIR 中的实际情况（对于 async coroutine）**：
    - **只生成 `Future::poll` 的实现**，没有独立的 `Coroutine::resume` 实现
    - **`Coroutine::resume` 方法在 MIR 中没有对应的代码**
    - 状态机逻辑在 `Future::poll` 内部实现，直接返回 `Poll::Pending` 或 `Poll::Ready`
    - 虽然在类型系统层面，async coroutine 类型同时实现了 `Coroutine` 和 `Future` 两个 trait，但在 MIR 代码生成阶段，编译器只生成 `Future::poll` 的实现
    - 根据 `compiler/rustc_mir_transform/src/coroutine.rs` 第 14-15 行的说明，编译器会根据 coroutine 类型选择生成 `Coroutine::resume` 或 `Future::poll`。对于 async coroutine，生成 `Future::poll`；对于其他类型的 coroutine（如 gen），可能生成 `Coroutine::resume`

- **关键理解**：
    - 虽然最终只生成了 `Future::poll` 的实现，但整个状态机的生成、变量提升、布局计算等核心机制都是基于 coroutine 的
    - Coroutine 提供了"暂停/恢复"的底层抽象，而 `Future::poll` 只是这个抽象在异步场景下的具体应用
    - 状态机结构体保存了跨暂停点的局部变量，使得协程可以在暂停后恢复执行
    - **关键转换**：在 MIR Transform 阶段（`compiler/rustc_mir_transform/src/coroutine.rs` 第 1864-1870 行），编译器已经将 `CoroutineState` 转换为 `Poll`，所以只需要生成一个 `Future::poll` 实现，返回类型直接是 `Poll<T>` 而不是 `CoroutineState<(), T>`

---

## 转换流程总结

```
源代码 (async fn + .await)
    ↓ [AST Lowering]
HIR (coroutine 闭包 + Future 类型抽象)
    ↓ [MIR Transform]
MIR (状态机 + Future::poll 实现)
    ↓ [代码生成]
LLVM IR → 机器码
```

**关键转换点**：

1. **HIR 阶段**：`async fn` → coroutine 闭包表达式 + `Future` 类型系统抽象
    - **实现机制**：coroutine 闭包（接收 `ResumeTy`，包含 `yield` 暂停点）
    - **类型系统**：返回类型是 `impl Future<Output = T>`（opaque type with Future bound）
    - **两者关系**：并存但分离 - coroutine 提供实现，Future 提供类型抽象

2. **MIR Transform 阶段**：将 coroutine 和 Future 统一为状态机 + `Future::poll` 实现
    - **状态机生成**：基于 coroutine 机制生成状态机结构体（discriminant、variant、变量提升）
    - **函数生成**：根据 coroutine 类型选择生成 `Future::poll` 或 `Coroutine::resume`
        - 对于 async coroutine：生成 `Future::poll`（返回 `Poll<T>`）
        - 对于 gen coroutine：生成 `Coroutine::resume`（返回 `CoroutineState<Y, R>`）
    - **转换映射**：
        - `yield ()` → 状态设置 + `Poll::Pending`（对于 async）
        - `return x` → 状态设置 + `Poll::Ready(x)`（对于 async）
        - `CoroutineState::Yielded(())` → `Poll::Pending`
        - `CoroutineState::Complete(x)` → `Poll::Ready(x)`

**理解要点**：

- **不是"混合"**，而是"统一"：HIR 阶段 coroutine 和 Future 是分离的（实现机制 vs 类型系统），MIR 阶段将它们统一为状态机 + `Future::poll` 实现
- **Coroutine 是底层机制**：提供"暂停/恢复"的能力，状态机的生成、变量提升等都基于 coroutine 机制
- **Future 是上层抽象**：类型系统层面的抽象，在 MIR 阶段通过生成 `Future::poll` 实现来体现
- **转换过程**：`StateTransform` pass（`compiler/rustc_mir_transform/src/coroutine.rs`）将 HIR 中的 coroutine 转换为 MIR 中的状态机，并根据 coroutine 类型选择生成 `Future::poll` 或 `Coroutine::resume`

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
- **状态 0（Unresumed）**：未开始，执行初始代码
- **状态 1（Returned）**：已完成，如果再次 poll 会 panic
- **状态 2（Panicked）**：已销毁（panic 后），如果再次 poll 会 panic
- **状态 3+（Suspend0, Suspend1, ...）**：暂停点，从该点恢复执行

**统一的状态命名规范**（与 `rust-async.md` 保持一致）：
- 状态 0：`Unresumed`（未开始）
- 状态 1：`Returned`（已完成）
- 状态 2：`Panicked`（已销毁）
- 状态 3+：`Suspend0`, `Suspend1`, `Suspend2`, ...（暂停点）

**注意**：编译器使用 `u32` 类型的 discriminant 来表示状态，而不是 Rust enum。状态名称（如 `Unresumed`、`Returned`、`Suspend0` 等）是编译器在调试时使用的名称。

## 注意事项

- 需要使用 nightly 工具链（`+nightly`）
- **HIR 和 MIR 都是编译器的内部数据结构**（不是 Rust 代码），输出是格式化后的文本表示
    - HIR：内存中的结构体/枚举集合，使用 arena 分配器
    - MIR：基于控制流图（CFG）的数据结构，包含基本块和边
- 输出可能很长，建议重定向到文件查看：
  ```bash
  cargo +nightly rustc -- -Zunpretty=hir > hir_output.txt
  cargo +nightly rustc -- -Zunpretty=mir > mir_output.txt
  ```
- 如果输出太长，可以使用 `less` 或编辑器查看：
  ```bash
  cargo +nightly rustc -- -Zunpretty=mir | less
  ```

## MIR 输出与 MIR 数据结构的对应关系

MIR 输出中的文本表示（如 `bb0`, `bb1`, `discriminant`, `switchInt` 等）对应了 MIR 的数据结构。以下是详细的映射关系：

### 核心数据结构

**文件**: `compiler/rustc_middle/src/mir/mod.rs`

#### 1. `Body<'tcx>` - 整个函数体

**定义**（第 210-307 行）：
```rust
pub struct Body<'tcx> {
    pub basic_blocks: BasicBlocks<'tcx>,      // 基本块集合
    pub local_decls: IndexVec<Local, LocalDecl<'tcx>>,  // 局部变量声明
    pub coroutine: Option<Box<CoroutineInfo<'tcx>>>,   // coroutine 信息
    pub arg_count: usize,                      // 参数数量
    // ... 其他字段
}
```

**对应关系**：
- MIR 输出中的整个函数（如 `fn foo::{closure#0}(...) -> Poll<i32> { ... }`）对应一个 `Body<'tcx>` 实例
- `basic_blocks` 字段包含了所有的基本块（`bb0`, `bb1`, `bb2`, ...）
- `local_decls` 字段包含了所有的局部变量声明（`_0`, `_1`, `_2`, ...）

#### 2. `BasicBlocks<'tcx>` - 基本块集合

**定义**（`compiler/rustc_middle/src/mir/basic_blocks.rs` 第 15 行）：
```rust
pub struct BasicBlocks<'tcx> {
    // IndexVec<BasicBlock, BasicBlockData<'tcx>>
}
```

**对应关系**：
- MIR 输出中的 `bb0: { ... }`, `bb1: { ... }` 等对应 `BasicBlocks` 中的元素
- `BasicBlock` 是一个索引类型（`rustc_index::newtype_index!`），用于索引基本块

#### 3. `BasicBlockData<'tcx>` - 单个基本块的数据

**定义**（第 1307-1330 行）：
```rust
pub struct BasicBlockData<'tcx> {
    pub statements: Vec<Statement<'tcx>>,     // 语句列表
    pub terminator: Option<Terminator<'tcx>>, // 终止符
    pub is_cleanup: bool,                      // 是否是 cleanup 块
}
```

**对应关系**：
- MIR 输出中的 `bb0: { ... }` 对应一个 `BasicBlockData<'tcx>` 实例
- `statements` 字段包含了基本块中的所有语句
- `terminator` 字段包含了基本块的终止符（如 `switchInt`, `goto`, `return` 等）

#### 4. `Statement<'tcx>` - 语句

**定义**（`compiler/rustc_middle/src/mir/statement.rs` 第 17 行）：
```rust
pub struct Statement<'tcx> {
    pub source_info: SourceInfo,
    pub kind: StatementKind<'tcx>,
    pub debuginfos: StmtDebugInfos<'tcx>,
}
```

**对应关系**：
- MIR 输出中的语句（如 `_3 = const 1_i32;`）对应一个 `Statement<'tcx>` 实例
- `kind` 字段是 `StatementKind<'tcx>` 枚举，包含不同类型的语句

#### 5. `StatementKind<'tcx>` - 语句类型

**定义**（`compiler/rustc_middle/src/mir/syntax.rs` 第 311 行）：
```rust
pub enum StatementKind<'tcx> {
    Assign(Box<(Place<'tcx>, Rvalue<'tcx>)>),  // 赋值语句
    SetDiscriminant { place: Box<Place<'tcx>>, variant_index: VariantIdx },  // 设置判别式
    StorageLive(Local),                         // 局部变量分配
    StorageDead(Local),                         // 局部变量释放
    // ... 其他类型
}
```

**对应关系**：
- `_3 = const 1_i32;` → `StatementKind::Assign(Place::from(_3), Rvalue::Use(Operand::Const(...)))`
- `discriminant((*_18)) = 3;` → `StatementKind::SetDiscriminant { place: ..., variant_index: 3 }`
- `StorageLive(_3);` → `StatementKind::StorageLive(Local::new(3))`

#### 6. `Terminator<'tcx>` - 终止符

**定义**（`compiler/rustc_middle/src/mir/terminator.rs` 第 436 行）：
```rust
pub struct Terminator<'tcx> {
    pub source_info: SourceInfo,
    pub kind: TerminatorKind<'tcx>,
}
```

**对应关系**：
- MIR 输出中的终止符（如 `switchInt(...)`, `goto -> bb5`, `return`）对应一个 `Terminator<'tcx>` 实例
- `kind` 字段是 `TerminatorKind<'tcx>` 枚举，包含不同类型的终止符

#### 7. `TerminatorKind<'tcx>` - 终止符类型

**定义**（`compiler/rustc_middle/src/mir/syntax.rs` 第 703 行）：
```rust
pub enum TerminatorKind<'tcx> {
    Goto { target: BasicBlock },                // 无条件跳转
    SwitchInt { discr: Operand<'tcx>, targets: SwitchTargets },  // 条件跳转
    Return,                                     // 返回
    Call { func: Operand<'tcx>, args: Vec<Spanned<Operand<'tcx>>>, ... },  // 函数调用
    // ... 其他类型
}
```

**对应关系**：
- `switchInt(move _17) -> [0: bb1, 1: bb15, ...]` → `TerminatorKind::SwitchInt { discr: Operand::Move(_17), targets: SwitchTargets { ... } }`
- `goto -> bb5;` → `TerminatorKind::Goto { target: BasicBlock::new(5) }`
- `return;` → `TerminatorKind::Return`
- `_7 = <Ready<i32> as Future>::poll(...)` → `TerminatorKind::Call { func: ..., args: ..., ... }`

#### 8. `Place<'tcx>` - 位置（内存位置表达式）

**定义**（`compiler/rustc_middle/src/mir/syntax.rs` 第 1189 行）：
```rust
pub struct Place<'tcx> {
    pub local: Local,
    pub projection: &'tcx List<PlaceElem<'tcx>>,
}
```

**对应关系**：
- `_3` → `Place { local: Local::new(3), projection: &[] }`
- `(((*_18) as variant#3).0: i32)` → `Place { local: Local::new(18), projection: &[Deref, Downcast(3), Field(0)] }`
- `_1.0` → `Place { local: Local::new(1), projection: &[Field(0)] }`

#### 9. `Rvalue<'tcx>` - 右值（产生值的表达式）

**定义**（`compiler/rustc_middle/src/mir/syntax.rs` 第 1358 行）：
```rust
pub enum Rvalue<'tcx> {
    Use(Operand<'tcx>),                         // 使用操作数
    BinaryOp(BinOp, Box<(Operand<'tcx>, Operand<'tcx>)>),  // 二元运算
    Discriminant(Place<'tcx>),                  // 获取判别式
    Aggregate(AggregateKind<'tcx>, Vec<Operand<'tcx>>),  // 聚合值（结构体、元组等）
    // ... 其他类型
}
```

**对应关系**：
- `const 1_i32` → `Rvalue::Use(Operand::Const(...))`
- `AddWithOverflow(copy _3, const 2_i32)` → `Rvalue::BinaryOp(BinOp::Add, ...)`
- `discriminant((*_18))` → `Rvalue::Discriminant(Place { local: Local::new(18), projection: &[Deref] })`
- `Poll::<i32>::Pending` → `Rvalue::Aggregate(AggregateKind::Adt(...), ...)`

#### 10. `LocalDecl<'tcx>` - 局部变量声明

**定义**（第 955-1020 行）：
```rust
pub struct LocalDecl<'tcx> {
    pub mutability: Mutability,
    pub ty: Ty<'tcx>,
    pub user_ty: Option<Box<UserTypeProjections>>,
    // ... 其他字段
}
```

**对应关系**：
- MIR 输出中的 `let mut _18: &mut {async fn body of foo()};` 对应 `local_decls[Local::new(18)]` 中的 `LocalDecl`
- `ty` 字段存储了变量的类型

#### 11. `CoroutineInfo<'tcx>` - Coroutine 相关信息

**定义**（第 150-192 行）：
```rust
pub struct CoroutineInfo<'tcx> {
    pub yield_ty: Option<Ty<'tcx>>,            // yield 类型（在 StateTransform 后为 None）
    pub resume_ty: Option<Ty<'tcx>>,           // resume 类型（在 StateTransform 后为 None）
    pub coroutine_drop: Option<Body<'tcx>>,     // coroutine drop shim
    pub coroutine_drop_async: Option<Body<'tcx>>,  // async drop shim
    pub coroutine_layout: Option<CoroutineLayout<'tcx>>,  // coroutine 布局
    pub coroutine_kind: CoroutineKind,         // coroutine 类型
}
```

**对应关系**：
- `Body<'tcx>` 的 `coroutine` 字段存储了 coroutine 相关信息
- `coroutine_layout` 字段存储了状态机的布局信息（在 StateTransform 后填充）

### 具体映射示例

基于实际的 MIR 输出，以下是具体的映射关系：

```rust
// MIR 输出：
bb0: {
    _18 = copy (_1.0: &mut {async fn body of foo()});
    _17 = discriminant((*_18));
    switchInt(move _17) -> [0: bb1, 1: bb15, 2: bb14, 3: bb13, otherwise: bb8];
}

// 对应的 MIR 数据结构：
Body {
    basic_blocks: BasicBlocks {
        [BasicBlock::new(0)] => BasicBlockData {
            statements: vec![
                Statement {
                    kind: StatementKind::Assign(Box::new((
                        Place { local: Local::new(18), projection: &[] },
                        Rvalue::Use(Operand::Copy(Place {
                            local: Local::new(1),
                            projection: &[Field(0)]
                        }))
                    )))
                },
                Statement {
                    kind: StatementKind::Assign(Box::new((
                        Place { local: Local::new(17), projection: &[] },
                        Rvalue::Discriminant(Place {
                            local: Local::new(18),
                            projection: &[Deref]
                        })
                    )))
                }
            ],
            terminator: Some(Terminator {
                kind: TerminatorKind::SwitchInt {
                    discr: Operand::Move(Place { local: Local::new(17), projection: &[] }),
                    targets: SwitchTargets {
                        values: vec![0, 1, 2, 3],
                        targets: vec![bb1, bb15, bb14, bb13],
                        otherwise: bb8
                    }
                }
            })
        },
        // ... 其他基本块
    },
    local_decls: IndexVec::from_elem_n(/* ... */),
    coroutine: Some(Box::new(CoroutineInfo { /* ... */ }))
}
```

### 关键映射点

1. **基本块**：`bb0`, `bb1`, ... → `BasicBlockData<'tcx>` 实例
2. **语句**：`_3 = const 1_i32;` → `Statement<'tcx>` 实例
3. **终止符**：`switchInt(...)`, `goto -> bb5`, `return` → `Terminator<'tcx>` 实例
4. **位置**：`_3`, `(((*_18) as variant#3).0: i32)` → `Place<'tcx>` 实例
5. **右值**：`const 1_i32`, `discriminant((*_18))` → `Rvalue<'tcx>` 枚举
6. **局部变量**：`_0`, `_1`, `_2`, ... → `LocalDecl<'tcx>` 实例
7. **状态机布局**：`variant#3` → `CoroutineLayout<'tcx>` 中的 variant 信息

### 总结

MIR 输出中的文本表示是这些数据结构的格式化输出。实际的 MIR 是内存中的数据结构（`Body`, `BasicBlockData`, `Statement`, `Terminator` 等），编译器在这些数据结构上进行借用检查、优化和代码生成。

## MIR 到二进制代码的编译流程

MIR 是编译器的中间表示，它需要经过多个阶段才能最终生成可执行的二进制代码。以下是完整的编译流程：

### 整体流程

```
MIR (中级中间表示)
    ↓ [1. Monomorphization Collection - 单态化收集]
收集需要生成代码的具体类型
    ↓ [2. Lowering MIR to LLVM IR - MIR 到 LLVM IR 的转换]
LLVM IR (低级中间表示)
    ↓ [3. LLVM Optimization Passes - LLVM 优化]
优化后的 LLVM IR
    ↓ [4. LLVM Code Generation - LLVM 代码生成]
目标平台汇编代码 / 机器码
    ↓ [5. Linking - 链接]
最终可执行二进制文件
```

### 详细步骤

#### 1. Monomorphization Collection（单态化收集）

**目的**：收集需要生成代码的具体类型，因为泛型代码需要为每个具体类型生成一份副本。

**位置**：`compiler/rustc_monomorphize/src/collector.rs`

**过程**：
- 遍历 MIR，找出所有需要单态化的项（mono items）
- 对于泛型函数 `fn foo<T>()`，如果被 `foo::<i32>()` 和 `foo::<u64>()` 调用，则需要为 `i32` 和 `u64` 各生成一份代码
- 生成一个"代码生成单元"（codegen unit）列表，每个单元包含一组相关的 mono items

**关键概念**：
- **Monomorphization（单态化）**：将泛型代码转换为具体类型的代码
- **Codegen Unit（代码生成单元）**：一组相关的 mono items，可以并行编译

#### 2. Lowering MIR to LLVM IR（MIR 到 LLVM IR 的转换）

**目的**：将 MIR 转换为 LLVM IR（低级中间表示），这是 LLVM 可以理解和优化的格式。

**入口函数**：`rustc_codegen_ssa::mir::codegen_mir`（`compiler/rustc_codegen_ssa/src/mir/mod.rs` 第 173 行）

**转换模块**：
- **`rustc_codegen_ssa::mir::block`**：转换基本块和终止符
    - 处理函数调用、异常处理（unwinding）
    - 生成 `switchInt`、`goto`、`return` 等终止符的 LLVM IR
- **`rustc_codegen_ssa::mir::statement`**：转换 MIR 语句
    - `Assign` → LLVM IR 赋值指令
    - `SetDiscriminant` → LLVM IR 判别式设置
    - `StorageLive`/`StorageDead` → LLVM IR 内存分配/释放
- **`rustc_codegen_ssa::mir::operand`**：转换操作数
    - `Operand::Copy` → LLVM IR 复制操作
    - `Operand::Move` → LLVM IR 移动操作
    - `Operand::Const` → LLVM IR 常量
- **`rustc_codegen_ssa::mir::place`**：转换位置引用
    - `Place` → LLVM IR 内存地址计算
    - 处理字段访问、数组索引、解引用等
- **`rustc_codegen_ssa::mir::rvalue`**：转换右值
    - `Rvalue::Use` → LLVM IR 值使用
    - `Rvalue::BinaryOp` → LLVM IR 二元运算
    - `Rvalue::Discriminant` → LLVM IR 判别式读取
    - `Rvalue::Aggregate` → LLVM IR 聚合值构造

**分析阶段**：
在转换之前，会运行一些分析 pass 来优化生成的 LLVM IR：
- **SSA 分析**（`rustc_codegen_ssa::mir::analyze`）：识别哪些变量是 SSA 形式的，可以直接转换为 SSA，而不需要依赖 LLVM 的 `mem2reg` pass
- **清理类型分析**：分析哪些基本块是 cleanup 块，用于异常处理

**映射关系**：
- 一个 MIR 基本块通常对应一个 LLVM 基本块
- 函数调用、断言等复杂操作可能生成多个 LLVM 基本块
- MIR 的 `switchInt` 直接映射到 LLVM 的 `switch` 指令

#### 3. LLVM Optimization Passes（LLVM 优化）

**目的**：对 LLVM IR 进行优化，提高代码性能。

**位置**：`compiler/rustc_llvm/llvm-wrapper/PassWrapper.cpp`

**优化级别**：
- **`-O0`（无优化）**：几乎不进行优化，编译速度快
- **`-O1`（基本优化）**：进行基本的优化，平衡编译速度和运行速度
- **`-O2`（标准优化）**：进行标准优化，通常用于发布版本
- **`-O3`（激进优化）**：进行更激进的优化，可能增加编译时间

**常见优化 Pass**：
- **`mem2reg`**：将内存操作转换为寄存器操作（SSA 形式）
- **`instcombine`**：指令合并，简化指令序列
- **`deadcodeelimination`**：死代码消除
- **`loop-unroll`**：循环展开
- **`inline`**：函数内联
- **`gvn`**（Global Value Numbering）：全局值编号，消除冗余计算

**LTO（Link-Time Optimization）**：
- **ThinLTO**：在链接时进行跨模块优化
- **FullLTO**：在链接时进行全局优化

#### 4. LLVM Code Generation（LLVM 代码生成）

**目的**：将优化后的 LLVM IR 转换为目标平台的汇编代码或机器码。

**位置**：LLVM 后端（`rustc_codegen_llvm` 调用 LLVM API）

**过程**：
1. **指令选择**：将 LLVM IR 指令映射到目标平台的机器指令
2. **寄存器分配**：将虚拟寄存器分配到物理寄存器
3. **指令调度**：重新排列指令顺序，提高指令级并行性
4. **代码发射**：生成目标平台的汇编代码或直接生成机器码（目标文件）

**输出格式**：
- **汇编代码**（`.s` 文件）：人类可读的汇编代码
- **目标文件**（`.o` 文件）：机器码，包含符号表和重定位信息
- **LLVM Bitcode**（`.bc` 文件）：LLVM IR 的二进制格式

#### 5. Linking（链接）

**目的**：将多个目标文件链接成最终的可执行文件或库。

**位置**：`compiler/rustc_codegen_ssa/src/back/write.rs` 和 `compiler/rustc_codegen_llvm/src/back/write.rs`

**过程**：
1. **符号解析**：解析所有未定义的符号引用
2. **重定位**：修正目标文件中的地址引用
3. **代码生成单元合并**：将多个代码生成单元的目标文件合并
4. **元数据链接**：链接 Rust 元数据（用于增量编译、文档生成等）
5. **最终输出**：
    - **可执行文件**（`a.out`、`.exe`）：可以直接运行的程序
    - **静态库**（`.a`、`.lib`）：包含所有代码的库文件
    - **动态库**（`.so`、`.dll`、`.dylib`）：运行时加载的库文件

**链接器**：
- **系统链接器**：`ld`（Unix）、`link.exe`（Windows）、`lld`（LLVM 链接器）
- **Rust 链接器**：可以使用自定义链接器（通过 `-C linker` 选项）

### 关键文件位置

#### Monomorphization Collection
- **`compiler/rustc_monomorphize/src/collector.rs`**：单态化收集器
- **`compiler/rustc_monomorphize/src/partitioning.rs`**：代码生成单元划分

#### MIR to LLVM IR Lowering
- **`compiler/rustc_codegen_ssa/src/mir/mod.rs`**：MIR 转换入口
- **`compiler/rustc_codegen_ssa/src/mir/block.rs`**：基本块和终止符转换
- **`compiler/rustc_codegen_ssa/src/mir/statement.rs`**：语句转换
- **`compiler/rustc_codegen_ssa/src/mir/operand.rs`**：操作数转换
- **`compiler/rustc_codegen_ssa/src/mir/place.rs`**：位置转换
- **`compiler/rustc_codegen_ssa/src/mir/rvalue.rs`**：右值转换
- **`compiler/rustc_codegen_ssa/src/mir/analyze.rs`**：分析 pass

#### LLVM 后端
- **`compiler/rustc_codegen_llvm/src/lib.rs`**：LLVM 后端入口
- **`compiler/rustc_codegen_llvm/src/back/write.rs`**：LLVM IR 写入和链接
- **`compiler/rustc_llvm/llvm-wrapper/PassWrapper.cpp`**：LLVM Pass 包装器

#### 后端抽象
- **`compiler/rustc_codegen_ssa/src/traits/backend.rs`**：后端 trait 定义
- **`compiler/rustc_codegen_ssa/src/base.rs`**：后端通用代码

### 查看中间产物

#### 查看 LLVM IR
```bash
# 生成 LLVM IR 文本格式
cargo rustc -- --emit=llvm-ir

# 生成 LLVM Bitcode
cargo rustc -- --emit=llvm-bc
```

#### 查看汇编代码
```bash
# 生成汇编代码
cargo rustc -- --emit=asm
```

#### 查看目标文件
```bash
# 生成目标文件（.o）
cargo rustc -- --emit=obj
```

### 多后端支持

Rust 编译器支持多个代码生成后端：

1. **LLVM 后端**（默认）：
    - 位置：`compiler/rustc_codegen_llvm`
    - 优点：优化能力强，支持平台广泛
    - 缺点：编译时间较长

2. **Cranelift 后端**（实验性）：
    - 位置：`compiler/rustc_codegen_cranelift`
    - 优点：编译速度快
    - 缺点：优化能力较弱，主要用于调试

3. **GCC 后端**（实验性）：
    - 位置：`compiler/rustc_codegen_gcc`
    - 优点：可以使用 GCC 的优化和工具链
    - 状态：仍在开发中

### 总结

MIR 到二进制代码的编译流程是一个多阶段的过程：

1. **单态化收集**：确定需要生成代码的具体类型
2. **MIR 到 LLVM IR**：将高级中间表示转换为低级中间表示
3. **LLVM 优化**：对 LLVM IR 进行各种优化
4. **代码生成**：将 LLVM IR 转换为目标平台的机器码
5. **链接**：将多个目标文件链接成最终的可执行文件

整个过程充分利用了 LLVM 的强大优化能力和多平台支持，同时保持了 Rust 的类型安全和内存安全特性。

## 相关资源

- [Rust 编译器开发指南 - HIR](https://rustc-dev-guide.rust-lang.org/hir.html)
- [Rust 编译器开发指南 - MIR](https://rustc-dev-guide.rust-lang.org/mir/index.html)
- [Rust 编译器开发指南 - Codegen](https://rustc-dev-guide.rust-lang.org/backend/codegen.html)
- [Rust 编译器开发指南 - Lowering MIR](https://rustc-dev-guide.rust-lang.org/backend/lowering-mir.html)
- [LLVM 官方文档](https://llvm.org/docs/)
- [Rust Playground](https://play.rust-lang.org/) - 可以在线查看 MIR 输出

