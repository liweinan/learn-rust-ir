# Rust async/await 编译器内部机制详解

## 前言

Rust 的 `async`/`await` 语法看起来非常简洁线性，但实际上在编译阶段会被彻底"去糖"（desugar）为一个手写状态机式的 `Future`。本文基于 Rust 编译器源代码，通过逐步添加代码的例子，展示编译器在不同情况下的行为差异，并提供具体的源代码位置和关键代码片段。

### 什么是"去糖"（Desugar）？

**去糖（Desugar）** 是编译原理中的一个重要概念，指的是将**语法糖（Syntactic Sugar）**转换为更底层、更明确的代码形式。

**语法糖**：为了让代码更易读、更简洁而提供的语法特性，它不引入新的功能，只是提供一种更友好的写法。

**去糖**：编译器将语法糖转换为等价的、更基础的代码形式。

**在 Rust async/await 中的例子**：

```rust
// 语法糖（用户写的代码）
async fn foo() {
   let x = 1;
   some_future().await;
   println!("{}", x);
}
```

编译器会将其"去糖"为：

```rust
// 去糖后的等价代码（编译器生成的）
fn foo() -> impl Future<Output = ()> {
    // 生成状态机结构体
    struct FooFuture {
        state: u32,
        x: i32,
        // ...
    }
    
    impl Future for FooFuture {
        fn poll(...) -> Poll<()> {
            // 状态机逻辑
            match self.state {
                0 => { /* 初始状态 */ }
                3 => { /* 从 .await 恢复 */ }
                // ...
            }
        }
    }
    
    FooFuture { state: 0, x: 0 }
}
```

**去糖的好处**：
1. **统一底层表示**：将不同的语法糖转换为统一的底层表示，简化编译器实现
2. **代码优化**：在统一的底层表示上进行优化，而不是为每种语法糖单独优化
3. **语义清晰**：去糖后的代码更明确地表达了实际的执行逻辑

**在 Rust 编译器中的去糖过程**：
- **AST Lowering 阶段**：将 `async fn` 和 `.await` 去糖为 coroutine 形式
- **MIR Transform 阶段**：将 coroutine 去糖为状态机

本文中的"手动去糖"指的是：为了说明编译器的行为，我们手动将 `async/await` 代码转换为等价的、更明确的代码形式，以便读者理解编译器实际生成的内容。

## 情况一：async fn 中没有 .await（只有同步代码）

**重要说明：`async fn` 和 `async block` 都生成 `Future`**

`async fn` 和 `async block` 的处理方式完全相同，区别仅在于：
- `async fn`：函数体被转换为一个 coroutine（`CoroutineSource::Fn`）
- `async block`：块表达式被转换为一个 coroutine（`CoroutineSource::Block`）
- `async closure`：闭包体被转换为一个 coroutine（`CoroutineSource::Closure`）

编译器使用相同的 `make_desugared_coroutine_expr` 函数处理它们（`compiler/rustc_ast_lowering/src/expr.rs` 第 704-792 行），最终都生成实现 `Future` trait 的类型。

```rust
async fn foo() {
    let x = 1; x
}
```

### 编译后大致等价于

```rust
fn foo() -> impl Future<Output = i32> {
    // 返回一个 Future，同步代码在第一次 poll 时执行
    struct FooFuture {
        state: State,
    }

    // 注意：这里的 enum State 只是手动去糖的示意
    // 实际编译器生成的是 u32 类型的 discriminant
    // 状态命名对应关系：
    // - Unresumed (状态 0)：未开始
    // - Returned (状态 1)：已完成
    // - Panicked (状态 2)：已销毁
    enum State {
        Unresumed,  // 状态 0：未开始
        Returned,   // 状态 1：已完成
        Panicked,   // 状态 2：已销毁
    }

    impl Future for FooFuture {
        type Output = i32;
        fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<i32> {
            match self.state {
                State::Unresumed => {  // 状态 0：未开始
                    // 所有同步代码在第一次 poll 时执行
                    let x = 1;
                    let result = x;  // 这里执行计算，结果在运行时确定
                    self.state = State::Returned;  // 状态 1：已完成
                    Poll::Ready(result)
                }
                State::Returned => panic!("future polled after completion"),  // 状态 1：已完成
                State::Panicked => panic!("future polled after panic"),      // 状态 2：已销毁
            }
        }
    }

    FooFuture { state: State::Unresumed }  // 状态 0：未开始
}
```

**注意**：即使没有 `.await`，编译器仍会生成一个简单的状态机（只有 Unresumed 和 Returned 两个状态，对应状态 0 和 1）。如果函数体包含复杂计算，这些计算会在第一次 `poll` 时执行，结果在运行时确定，而不是编译时。

### 特点
- 即使没有 `.await`，编译器仍会生成一个简单的状态机（只有 Unresumed 和 Returned 两个状态，对应状态 0 和 1）。
- 所有同步代码在第一次 `poll` 时执行完毕，计算结果在运行时确定。
- `foo().await` 会立即完成，几乎无运行时开销（但计算本身的开销仍然存在）。

### 源代码实现

在 AST Lowering 阶段，`async fn` 被转换为 coroutine 表达式。即使没有 `.await`，编译器仍会生成 coroutine，但可能被后续优化阶段优化为几乎零成本。

**文件**: `compiler/rustc_ast_lowering/src/item.rs`（第 1573-1593 行）

```rust
        let desugaring_kind = match coroutine_kind {
            CoroutineKind::Async { .. } => hir::CoroutineDesugaring::Async,
            CoroutineKind::Gen { .. } => hir::CoroutineDesugaring::Gen,
            CoroutineKind::AsyncGen { .. } => hir::CoroutineDesugaring::AsyncGen,
        };
        let closure_id = coroutine_kind.closure_id();

        let coroutine_expr = self.make_desugared_coroutine_expr(
            CaptureBy::Ref,
            closure_id,
            None,
            fn_decl_span,
            body_span,
            desugaring_kind,
            coroutine_source,
            mkbody,
        );
```

**转换后的结构**：`async fn foo() -> T { body }` 被转换为一个 coroutine 闭包表达式，大致等价于：

```rust
fn foo() -> impl Future<Output = T> {
    static move |_task_context: ResumeTy| -> T {
        // 函数体 body 的内容
    }
}
```

对于 async 函数，`make_desugared_coroutine_expr` 函数会添加 `ResumeTy` 参数和 `_task_context` 变量（`compiler/rustc_ast_lowering/src/expr.rs` 第 720-751 行）：

```rust
        let (inputs, params, task_context): (&[_], &[_], _) = match desugaring_kind {
            hir::CoroutineDesugaring::Async | hir::CoroutineDesugaring::AsyncGen => {
                // Resume argument type: `ResumeTy`
                let resume_ty =
                    self.make_lang_item_qpath(hir::LangItem::ResumeTy, unstable_span, None);
                // ... 创建 ResumeTy 参数 ...
                
                // Lower the argument pattern/ident. The ident is used again in the `.await` lowering.
                let (pat, task_context_hid) = self.pat_ident_binding_mode(
                    span,
                    Ident::with_dummy_span(sym::_task_context),
                    hir::BindingMode::MUT,
                );
                // ... 创建 _task_context 参数 ...
                
                (inputs, params, Some(task_context_hid))
            }
            hir::CoroutineDesugaring::Gen => (&[], &[], None),
        };
```

`ResumeTy` 是一个 lang item，定义在 `library/core/src/future/mod.rs`（第 47-57 行）：

```rust
#[lang = "ResumeTy"]
#[doc(hidden)]
#[unstable(feature = "gen_future", issue = "none")]
#[derive(Debug, Copy, Clone)]
pub struct ResumeTy(NonNull<Context<'static>>);

#[unstable(feature = "gen_future", issue = "none")]
unsafe impl Send for ResumeTy {}

#[unstable(feature = "gen_future", issue = "none")]
unsafe impl Sync for ResumeTy {}
```

## 情况二：引入 .await（出现真正的暂停点）

```rust
async fn foo() -> i32 {
    let x = 1;
    let y = x + 2;              // 同步操作，y == 3
    
    // 在 .await 之前，y 的值是 3
    // 这个值需要在暂停时被保存，因为后续代码会使用它
    futures::future::ready(42).await;  // 暂停点
    
    // .await 完成后，y 的值（3）被恢复，继续使用
    let result = y + 10;        // result = 3 + 10 = 13
    result                       // 返回 result
}
```

**关于 `futures::future::ready(42)`**：

`futures::future::ready(42)` 是一个返回 `impl Future<Output = i32>` 的函数。它可以是：
- 另一个 `async fn` 的调用
- `async` 块的调用（等价的写法：`async { 42 }.await`）
- 任何实现了 `Future` trait 的类型（如 `futures::future::Ready<T>`、`futures::future::Pending` 等）

**`.await` 的去糖过程**：

`futures::future::ready(42).await` 会被去糖为以下伪 Rust 代码：

```rust
// 1. 首先调用 IntoFuture::into_future 将表达式转换为 Future
//    对于 ready(42)，IntoFuture::into_future 直接返回它本身
match IntoFuture::into_future(futures::future::ready(42)) {
    mut __awaitee => loop {
        // 2. 在循环中调用 Future::poll
        match unsafe { Future::poll(
            Pin::new_unchecked(&mut __awaitee),
            get_context(_task_context),
        ) } {
            // 3. 如果 Future 完成，跳出循环，继续执行后续代码
            Poll::Ready(result) => break result,
            // 4. 如果 Future 未完成，暂停协程（yield）
            Poll::Pending => {}
        }
        // 5. 暂停点：yield ()，保存当前状态
        _task_context = yield ();
    }
}
```

**关键点**：
- `__awaitee` 是编译器生成的临时变量，用于保存正在等待的 Future
- `_task_context` 是协程的参数，类型为 `ResumeTy`，用于传递 `Context`
- `yield ()` 是协程的暂停点，会保存当前状态并返回 `CoroutineState::Yielded(())`
- 当 Future 完成时，`poll` 返回 `Poll::Ready(result)`，协程恢复执行后续代码

**实际编译器代码位置**：

**文件**: `compiler/rustc_ast_lowering/src/expr.rs`（第 829-1042 行）

编译器在 `lower_expr_await` 和 `make_lowered_await` 函数中实现 `.await` 的去糖逻辑。

### 编译后大致等价于（手动去糖）

编译器生成的类型**同时实现了 `Coroutine` 和 `Future` 两个 trait**。完整的实现如下：

```rust
// 注意：这里的 enum State 只是手动去糖的示意
// 实际编译器生成的是 u32 类型的 discriminant，而不是 Rust enum
// 定义位置：compiler/rustc_middle/src/ty/sty.rs（第 127-128 行）
// fn discr_ty(&self, tcx: TyCtxt<'tcx>) -> Ty<'tcx> {
//     tcx.types.u32
// }
// 注意：这里的 enum State 只是手动去糖的示意
// 实际编译器生成的是 u32 类型的 discriminant，而不是 Rust enum
// 状态命名对应关系：
// - Unresumed (状态 0)：未开始
// - Returned (状态 1)：已完成
// - Panicked (状态 2)：已销毁
// - Suspend0, Suspend1, ... (状态 3+)：暂停点
enum State {
    Unresumed,                          // 状态 0：未开始（Unresumed）
    Returned,                           // 状态 1：已完成（Returned）
    Panicked,                           // 状态 2：已销毁（Panicked）
    Suspend0(Ready<i32>),               // 状态 3：第一个暂停点（Suspend0）
}

struct FooFuture {
    state: State,  // 实际编译器生成的是 state: u32
    y: i32,  // 需要跨 .await 存活的变量被提升到这里
}
```

**关于状态枚举的说明**：

**重要**：文档中的 `enum State` 只是手动去糖的示意，实际编译器生成的是 `u32` 类型的 discriminant（判别值），而不是 Rust enum。

**状态命名和规则**：

- **状态 0**：`Unresumed`（未开始）- 协程尚未被恢复
- **状态 1**：`Returned`（已完成）- 协程已返回/完成
- **状态 2**：`Panicked`（已销毁）- 协程在 panic 时被销毁
- **状态 3+**：`Suspend0`, `Suspend1`, `Suspend2`, ...（暂停点）- 每个 `.await` 对应一个暂停点状态，按出现顺序从 3 开始递增

**Discriminant 类型定义**：

**文件**: `compiler/rustc_middle/src/ty/sty.rs`（第 127-128 行）

```rust
/// The type of the state discriminant used in the coroutine type.
#[inline]
fn discr_ty(&self, tcx: TyCtxt<'tcx>) -> Ty<'tcx> {
    tcx.types.u32  // 返回 u32 类型
}
```

这个方法属于 `CoroutineArgs` trait 的实现，明确指定了 coroutine 状态使用 `u32` 类型作为 discriminant。状态名称（如 `Unresumed`、`Returned`、`Suspend0` 等）是编译器在调试时使用的名称（见 `compiler/rustc_middle/src/ty/sty.rs` 第 116-123 行的 `variant_name` 函数）。

```rust
// Coroutine::resume 包含实际的状态机逻辑
impl Coroutine<ResumeTy> for FooFuture {
    type Yield = ();
    type Return = i32;

    fn resume(mut self: Pin<&mut Self>, arg: ResumeTy) -> CoroutineState<(), i32> {
        // ResumeTy 提取 Context 的详细过程：
        //
        // 1. ResumeTy 的定义（library/core/src/future/mod.rs 第 47-51 行）：
        //    pub struct ResumeTy(NonNull<Context<'static>>);
        //    - ResumeTy 是一个包装类型，内部存储一个指向 Context 的裸指针（NonNull）
        //    - 使用 'static 生命周期来绕过生命周期检查（因为协程需要存储 Context 的引用）
        //
        // 2. 为什么需要 ResumeTy？
        //    - Coroutine trait 不能直接使用 &mut Context<'a>，因为生命周期问题
        //    - 使用裸指针可以绕过生命周期检查，但需要 unsafe 操作
        //    - ResumeTy 实现了 Send 和 Sync，使得 Future 可以是 Send/Sync 的
        //
        // 3. get_context 的实现（library/core/src/future/mod.rs 第 64-68 行）：
        //    pub unsafe fn get_context<'a, 'b>(cx: ResumeTy) -> &'a mut Context<'b> {
        //        unsafe { &mut *cx.0.as_ptr().cast() }
        //    }
        //    - 从 ResumeTy 中提取内部的 NonNull 指针
        //    - 将裸指针转换为 &mut Context<'b>（unsafe 操作）
        //    - 调用者必须保证指针的有效性和生命周期正确性
        //
        // 4. 转换过程（编译器在不同阶段的处理）：
        //
        // 【阶段一：HIR（AST Lowering）阶段】
        // 文件：compiler/rustc_ast_lowering/src/expr.rs（第 720-751 行）
        //
        // 在创建 async coroutine 时，编译器会添加 ResumeTy 类型的参数：
        // ```rust
        // let resume_ty = self.make_lang_item_qpath(hir::LangItem::ResumeTy, ...);
        // let input_ty = hir::Ty {
        //     kind: hir::TyKind::Path(resume_ty),  // 参数类型为 ResumeTy
        //     ...
        // };
        // ```
        //
        // 在 .await 转换时（第 932-936 行），编译器生成 get_context 调用：
        // ```rust
        // let task_context = self.expr_ident_mut(span, task_context_ident, task_context_hid);
        // let get_context = self.expr_call_lang_item_fn_mut(
        //     gen_future_span,
        //     hir::LangItem::GetContext,  // 调用 get_context
        //     arena_vec![self; task_context],  // 传入 ResumeTy 参数
        // );
        // ```
        // 生成的代码类似：`get_context(_task_context)`，其中 `_task_context` 是 `ResumeTy` 类型
        //
        // 【阶段二：MIR Transform 阶段】
        // 文件：compiler/rustc_mir_transform/src/coroutine.rs（第 591-622 行）
        //
        // `transform_async_context` 函数将 ResumeTy 替换为 &mut Context<'_>：
        // ```rust
        // fn transform_async_context<'tcx>(tcx: TyCtxt<'tcx>, body: &mut Body<'tcx>) -> Ty<'tcx> {
        //     let context_mut_ref = Ty::new_task_context(tcx);  // 创建 &mut Context<'_> 类型
        //
        //     // 替换 resume 参数的类型（第 595 行）
        //     replace_resume_ty_local(tcx, body, CTX_ARG, context_mut_ref);
        //
        //     // 消除 get_context 调用（第 606-613 行）
        //     for bb in body.basic_blocks.indices() {
        //         match &bb_data.terminator().kind {
        //             TerminatorKind::Call { func, .. } => {
        //                 if func_ty == get_context_def_id {
        //                     let local = eliminate_get_context_call(&mut body[bb]);
        //                     // 将 get_context 的返回值类型也替换为 &mut Context<'_>
        //                     replace_resume_ty_local(tcx, body, local, context_mut_ref);
        //                 }
        //             }
        //         }
        //     }
        // }
        // ```
        //
        // `eliminate_get_context_call` 函数（第 624-641 行）：
        // ```rust
        // fn eliminate_get_context_call<'tcx>(bb_data: &mut BasicBlockData<'tcx>) -> Local {
        //     // 移除 get_context 函数调用
        //     // 直接将 ResumeTy 参数转换为 &mut Context<'_>
        //     // 因为此时类型已经统一为 &mut Context<'_>
        // }
        // ```
        //
        // 【最终生成的代码】
        // 在最终的 MIR 和生成的机器码中：
        // - Future::poll 接收 &mut Context<'_>
        // - 直接传递给 Coroutine::resume（不再需要 ResumeTy 转换）
        // - 不再有 get_context 调用
        //
        // 总结：ResumeTy 只在 HIR 阶段用于类型检查和借用检查，在 MIR 阶段就被替换为
        // &mut Context<'_>，最终生成的代码直接使用 &mut Context<'_>。
        //
        // 5. 安全性：
        //    - 这是一个 unsafe 操作，但编译器保证在协程执行期间 Context 是有效的
        //    - Context 的生命周期由运行时保证，在协程暂停期间不会被销毁
        let cx = unsafe { get_context(arg) };  // 从 ResumeTy 提取 Context
        
        // 注意：在这个例子中，每个分支都有 return，所以不需要 loop
        // 但在更复杂的情况下（如多个连续的 await，且前面的 await 立即完成），
        // 编译器可能会生成 loop 来在同一个 resume 调用中处理多个状态转换
        //
        // 示例：如果有多个 await，且前面的 await 立即完成
        // async fn example() {
        //     ready(1).await;  // 如果立即完成，继续执行
        //     ready(2).await;  // 如果立即完成，继续执行
        //     ready(3).await;  // 如果立即完成，继续执行
        // }
        // 在这种情况下，编译器可能会生成 loop，在同一个 resume 调用中：
        // loop {
        //     match self.state {
        //         State::Unresumed => {  // 状态 0：未开始
        //             match ready(1).poll(cx) {
        //                 Poll::Ready(_) => {
        //                     // 第一个 await 立即完成，设置状态为 Suspend0，继续循环
        //                     self.state = State::Suspend0(...);  // 状态 3
        //                     continue;  // 继续循环，下次循环会进入 State::Suspend0 分支
        //                 }
        //                 Poll::Pending => {
        //                     // 第一个 await 未完成，保存状态并暂停
        //                     self.state = State::Suspend0(...);  // 状态 3
        //                     return CoroutineState::Yielded(());
        //                 }
        //             }
        //         }
        //         State::Suspend0(...) => {  // 状态 3：第一个暂停点
        //             match ready(2).poll(cx) {
        //                 Poll::Ready(_) => {
        //                     // 第二个 await 立即完成，设置状态为 Suspend1，继续循环
        //                     self.state = State::Suspend1(...);  // 状态 4
        //                     continue;  // 继续循环，下次循环会进入 State::Suspend1 分支
        //                 }
        //                 Poll::Pending => {
        //                     // 第二个 await 未完成，保存状态并暂停
        //                     self.state = State::Suspend1(...);  // 状态 4
        //                     return CoroutineState::Yielded(());
        //                 }
        //             }
        //         }
        //         State::Suspend1(...) => {  // 状态 4：第二个暂停点
        //             // 处理第三个 await...
        //         }
        //         // ...
        //     }
        // }
        match self.state {
                State::Unresumed => {  // 状态 0：未开始
                    // y 的用途说明：
                    // 1. y 在 .await 之前被计算（y = x + 2 = 3）
                    // 2. y 在 .await 之后继续被使用（用于计算 result = y + 10）
                    // 3. 因此 y 必须被"提升"到 FooFuture 结构体中保存
                    // 4. 这样在协程暂停（yield）和恢复（resume）时，y 的值才能被正确保存和恢复
                    let x = 1;
                    self.y = x + 2;  // y == 3（在 .await 之前，y 的值是 3）

                    let mut sub = futures::future::ready(42);
                    match Pin::new(&mut sub).poll(cx) {
                        Poll::Ready(_awaited_value) => {
                            // 子 Future 立即完成，继续往下执行
                            // 使用保存的 y 值（3）继续计算
                            let result = self.y + 10;  // result = 3 + 10 = 13
                            self.state = State::Returned;  // 状态 1：已完成
                            return CoroutineState::Complete(result);  // 返回 result
                        }
                        Poll::Pending => {
                            // 子 Future 未完成，保存状态并暂停
                            // 此时 self.y == 3，这个值会被保存在 FooFuture 结构体中
                            // 当协程恢复时，可以从 self.y 中恢复这个值，用于后续的计算
                            self.state = State::Suspend0(sub);  // 状态 3：第一个暂停点
                            return CoroutineState::Yielded(());
                        }
                    }
                }

                State::Suspend0(ref mut sub) => {  // 状态 3：第一个暂停点
                    match Pin::new(sub).poll(cx) {
                        Poll::Ready(_awaited_value) => {
                            // 子 Future 完成，继续执行后续代码
                            // 注意：此时 self.y 的值仍然是 3（从 State::Unresumed 中保存的值）
                            // 因为 y 被提升到了 FooFuture 结构体中，所以在暂停和恢复之间，y 的值被正确保存
                            // 现在可以使用这个保存的值继续计算
                            let result = self.y + 10;  // result = 3 + 10 = 13
                            self.state = State::Returned;  // 状态 1：已完成
                            return CoroutineState::Complete(result);  // 返回 result
                        }
                        Poll::Pending => {
                            // 仍然未完成，继续暂停
                            // y 的值（3）仍然保存在 self.y 中，等待下次恢复
                            return CoroutineState::Yielded(());
                        }
                    }
                }

                State::Returned => panic!("future polled after completion"),  // 状态 1：已完成
                State::Panicked => panic!("future polled after panic"),      // 状态 2：已销毁
            }
    }
}

// Future::poll 调用 Coroutine::resume 并转换结果
impl Future for FooFuture {
    type Output = i32;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<i32> {
        // 注意：在实际的编译器生成代码中，Future::poll 直接传递 &mut Context<'_>
        // 给 Coroutine::resume，而不是 ResumeTy。
        //
        // 【编译器生成 Future::poll 签名的过程】
        // 文件：compiler/rustc_ty_utils/src/abi.rs（第 149-174 行）
        //
        // 编译器在确定 coroutine 的 ABI 时，会为 async coroutine 生成 Future::poll 签名：
        // ```rust
        // let (resume_ty, ret_ty) = match coroutine_kind {
        //     hir::CoroutineKind::Desugared(hir::CoroutineDesugaring::Async, _) => {
        //         // The signature should be `Future::poll(_, &mut Context<'_>) -> Poll<Output>`
        //         // 返回类型：Poll<Output>
        //         let ret_ty = Ty::new_adt(tcx, poll_adt_ref, poll_args);
        //
        //         // 将 ResumeTy 替换为 &mut Context<'_>
        //         // We have to replace the `ResumeTy` that is used for type and borrow checking
        //         // with `&mut Context<'_>` which is used in codegen.
        //         let context_mut_ref = Ty::new_task_context(tcx);
        //
        //         (Some(context_mut_ref), ret_ty)  // resume_ty 是 &mut Context<'_>
        //     }
        // };
        // ```
        //
        // 【转换过程（在编译器的不同阶段）】
        // 1. HIR 阶段：使用 ResumeTy 作为类型（用于类型检查和借用检查）
        // 2. MIR Transform 阶段：transform_async_context 函数将 ResumeTy 替换为 &mut Context<'_>
        //    （compiler/rustc_mir_transform/src/coroutine.rs 第 591-622 行）
        // 3. 最终生成的代码：直接使用 &mut Context<'_>
        //
        // 这里的手动去糖代码为了说明概念，展示了 ResumeTy 的使用，但实际生成的代码
        // 在 MIR 阶段就已经将 ResumeTy 替换为 &mut Context<'_> 了。
        let resume_arg = ResumeTy::from_context(cx);  // 仅用于说明，实际代码中直接传递 cx
        
        // 调用 Coroutine::resume，执行状态机逻辑
        match self.resume(resume_arg) {
            CoroutineState::Yielded(_) => Poll::Pending,  // 协程暂停 → Future 未完成
            CoroutineState::Complete(result) => Poll::Ready(result),  // 协程完成 → Future 完成，返回 x 的值
        }
    }
}

fn foo() -> FooFuture {
    FooFuture { state: State::Unresumed, x: 0 }  // 状态 0：未开始
}
```

**关键点**：

1. **`Coroutine::resume` 是核心实现**：包含完整的状态机逻辑，根据当前状态执行代码，遇到 `.await` 时返回 `CoroutineState::Yielded(())` 暂停，完成时返回 `CoroutineState::Complete(result)`。

2. **`Future::poll` 是对 `Coroutine::resume` 的包装**：
   - 将 `&mut Context<'_>` 转换为 `ResumeTy`
   - 调用 `Coroutine::resume` 执行状态机
   - 将 `CoroutineState` 转换为 `Poll`：
      - `CoroutineState::Yielded(_)` → `Poll::Pending`
      - `CoroutineState::Complete(x)` → `Poll::Ready(x)`

3. **调用链**：`Future::poll` → `Coroutine::resume` → 状态机逻辑 → 返回 `CoroutineState` → 转换为 `Poll`

## 情况三：嵌套的多层 .await

当有多个 `.await` 时，编译器会为每个 `.await` 生成一个状态。如果 `.await` 是嵌套的（一个 `.await` 在另一个 `.await` 的结果上），状态机会变得更加复杂。

### 示例代码

```rust
async fn nested_await() -> i32 {
    let x = 1;
    
    // 第一个 await
    let result1 = future1().await;  // 状态 3
    
    // 第二个 await（使用第一个 await 的结果）
    let result2 = future2(result1).await;  // 状态 4
    
    // 第三个 await（嵌套在第二个 await 的结果上）
    let result3 = future3(result2).await;  // 状态 5
    
    x + result3
}
```

### 编译后大致等价于（手动去糖）

```rust
// 注意：这里的 enum State 只是手动去糖的示意
// 实际编译器生成的是 u32 类型的 discriminant，而不是 Rust enum
enum State {
    Unresumed,                          // 状态 0：未开始（Unresumed）
    Returned,                           // 状态 1：已完成（Returned）
    Panicked,                           // 状态 2：已销毁（Panicked）
    Suspend0(Future1),                  // 状态 3：第一个暂停点（Suspend0），等待 future1
    Suspend1(Future1Result, Future2),   // 状态 4：第二个暂停点（Suspend1），等待 future2（需要保存 future1 的结果）
    Suspend2(Future1Result, Future2Result, Future3),  // 状态 5：第三个暂停点（Suspend2），等待 future3（需要保存前两个的结果）
}

struct NestedAwaitFuture {
    state: State,
    x: i32,  // 跨所有 await 存活的变量
}

impl Coroutine<ResumeTy> for NestedAwaitFuture {
    type Yield = ();
    type Return = i32;

    fn resume(mut self: Pin<&mut Self>, arg: ResumeTy) -> CoroutineState<(), i32> {
        let cx = unsafe { get_context(arg) };
        
        match self.state {
            State::Unresumed => {  // 状态 0：未开始
                self.x = 1;
                
                // 第一个 await
                let mut fut1 = future1();
                match Pin::new(&mut fut1).poll(cx) {
                    Poll::Ready(result1) => {
                        // future1 立即完成，继续执行第二个 await
                        let mut fut2 = future2(result1);
                        match Pin::new(&mut fut2).poll(cx) {
                            Poll::Ready(result2) => {
                                // future2 也立即完成，继续执行第三个 await
                                let mut fut3 = future3(result2);
                                match Pin::new(&mut fut3).poll(cx) {
                                    Poll::Ready(result3) => {
                                        // 所有 await 都立即完成
                                        self.state = State::Returned;  // 状态 1：已完成
                                        return CoroutineState::Complete(self.x + result3);
                                    }
                                    Poll::Pending => {
                                        // future3 未完成，保存所有中间结果
                                        self.state = State::Suspend2(result1, result2, fut3);  // 状态 5
                                        return CoroutineState::Yielded(());
                                    }
                                }
                            }
                            Poll::Pending => {
                                // future2 未完成，保存 result1 和 fut2
                                self.state = State::Suspend1(result1, fut2);  // 状态 4
                                return CoroutineState::Yielded(());
                            }
                        }
                    }
                    Poll::Pending => {
                        // future1 未完成，保存状态并暂停
                        self.state = State::Suspend0(fut1);  // 状态 3
                        return CoroutineState::Yielded(());
                    }
                }
            }
            
            State::Suspend0(ref mut fut1) => {  // 状态 3：第一个暂停点
                // 从第一个 await 恢复
                match Pin::new(fut1).poll(cx) {
                    Poll::Ready(result1) => {
                        // future1 完成，继续执行第二个 await
                        let mut fut2 = future2(result1);
                        match Pin::new(&mut fut2).poll(cx) {
                            Poll::Ready(result2) => {
                                // future2 也立即完成，继续执行第三个 await
                                let mut fut3 = future3(result2);
                                match Pin::new(&mut fut3).poll(cx) {
                                    Poll::Ready(result3) => {
                                        // 所有 await 都完成
                                        self.state = State::Returned;  // 状态 1：已完成
                                        return CoroutineState::Complete(self.x + result3);
                                    }
                                    Poll::Pending => {
                                        // future3 未完成，保存所有中间结果
                                        self.state = State::Suspend2(result1, result2, fut3);  // 状态 5
                                        return CoroutineState::Yielded(());
                                    }
                                }
                            }
                            Poll::Pending => {
                                // future2 未完成，保存 result1 和 fut2
                                self.state = State::Suspend1(result1, fut2);  // 状态 4
                                return CoroutineState::Yielded(());
                            }
                        }
                    }
                    Poll::Pending => {
                        // future1 仍然未完成
                        return CoroutineState::Yielded(());
                    }
                }
            }
            
            State::Suspend1(result1, ref mut fut2) => {  // 状态 4：第二个暂停点
                // 从第二个 await 恢复
                match Pin::new(fut2).poll(cx) {
                    Poll::Ready(result2) => {
                        // future2 完成，继续执行第三个 await
                        let mut fut3 = future3(result2);
                        match Pin::new(&mut fut3).poll(cx) {
                            Poll::Ready(result3) => {
                                // 所有 await 都完成
                                self.state = State::Returned;  // 状态 1：已完成
                                return CoroutineState::Complete(self.x + result3);
                            }
                            Poll::Pending => {
                                // future3 未完成，保存所有中间结果
                                self.state = State::Suspend2(result1, result2, fut3);  // 状态 5
                                return CoroutineState::Yielded(());
                            }
                        }
                    }
                    Poll::Pending => {
                        // future2 仍然未完成
                        return CoroutineState::Yielded(());
                    }
                }
            }
            
            State::Suspend2(_result1, _result2, ref mut fut3) => {  // 状态 5：第三个暂停点
                // 从第三个 await 恢复
                match Pin::new(fut3).poll(cx) {
                    Poll::Ready(result3) => {
                        // future3 完成，所有 await 都完成
                        self.state = State::Returned;  // 状态 1：已完成
                        return CoroutineState::Complete(self.x + result3);
                    }
                    Poll::Pending => {
                        // future3 仍然未完成
                        return CoroutineState::Yielded(());
                    }
                }
            }
            
            State::Returned => panic!("future polled after completion"),  // 状态 1：已完成
            State::Panicked => panic!("future polled after panic"),        // 状态 2：已销毁
        }
    }
}

impl Future for NestedAwaitFuture {
    type Output = i32;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<i32> {
        let resume_arg = ResumeTy::from_context(cx);
        match self.resume(resume_arg) {
            CoroutineState::Yielded(_) => Poll::Pending,
            CoroutineState::Complete(result) => Poll::Ready(result),
        }
    }
}
```

### 关键观察点

1. **状态数量**：每个 `.await` 对应一个状态（状态 3、4、5），加上保留状态（0、1、2），总共 6 个状态。
   - 状态 0：`Unresumed`（未开始）
   - 状态 1：`Returned`（已完成）
   - 状态 2：`Panicked`（已销毁）
   - 状态 3：`Suspend0`（第一个暂停点）
   - 状态 4：`Suspend1`（第二个暂停点）
   - 状态 5：`Suspend2`（第三个暂停点）

2. **状态中保存的数据**：
   - `State::Suspend0`：只保存 `Future1`（状态 3）
   - `State::Suspend1`：保存 `Future1` 的结果和 `Future2`（状态 4，因为 `future2` 需要 `result1`）
   - `State::Suspend2`：保存 `Future1` 的结果、`Future2` 的结果和 `Future3`（状态 5，因为 `future3` 需要 `result2`）

3. **变量提升**：
   - `x` 被提升到 `NestedAwaitFuture` 结构体中，因为它跨越了所有 `.await`
   - 中间结果（`result1`、`result2`）被保存在状态枚举中，因为它们需要在后续的 `.await` 中使用

4. **状态转换流程**：
   ```
   Unresumed (0) 
     → Suspend0 (3) [如果 future1 未完成]
     → Suspend1 (4) [如果 future2 未完成]
     → Suspend2 (5) [如果 future3 未完成]
     → Returned (1)
   ```

5. **嵌套 await 的特点**：
   - 每个状态需要保存**所有之前 await 的结果**，因为后续的 await 可能依赖这些结果
   - 状态枚举的大小会随着嵌套深度和中间结果的数量增长
   - 编译器会自动分析哪些值需要保存在状态中

### 函数调用嵌套的 await

如果 await 是嵌套在函数调用中的，比如：

```rust
async fn example() -> i32 {
    // 第一个 await：等待 async_fn3() 完成，结果传递给 async_fn2
    let result3 = async_fn3().await;
    
    // 第二个 await：等待 async_fn2(result3) 完成，结果传递给 async_fn1
    let result2 = async_fn2(result3).await;
    
    // 第三个 await：等待 async_fn1(result2) 完成，返回结果
    async_fn1(result2).await
}
```

或者更简洁的写法（`.await` 的优先级高于函数调用）：

```rust
async fn example() -> i32 {
    async_fn1(
        async_fn2(
            async_fn3().await  // 第一个 await：async_fn3().await 的结果传递给 async_fn2
        ).await                // 第二个 await：async_fn2(...).await 的结果传递给 async_fn1
    ).await                    // 第三个 await：async_fn1(...).await 的结果作为返回值
}
```

**处理方式完全相同**。编译器仍然会为每个 `.await` 生成一个状态，无论 await 的是：
- 直接的 Future：`future().await`
- 函数调用的结果：`async_fn().await`
- 嵌套函数调用的结果：`async_fn1(async_fn2().await).await`

**原因**：
- 编译器在 AST Lowering 阶段会将所有 `.await` 转换为相同的模式（`loop { match poll(...) { ... } }`）
- 每个 `.await` 都会生成一个 `yield` 点
- 每个 `yield` 点都会分配一个唯一的状态编号
- 函数调用的嵌套不会影响状态机的生成逻辑

**示例**：上面的代码会生成与"情况三"相同的状态结构：
- 状态 3：等待 `async_fn3().await`
- 状态 4：等待 `async_fn2(...).await`（需要保存 `async_fn3` 的结果）
- 状态 5：等待 `async_fn1(...).await`（需要保存 `async_fn2` 的结果）

### 源代码实现

编译器处理嵌套 await 的逻辑与单个 await 相同：

**文件**: `compiler/rustc_mir_transform/src/coroutine.rs`（第 460-496 行）

每个 `.await`（yield 点）都会：
1. 分配一个唯一的状态编号（从 3 开始递增）
2. 分析哪些局部变量需要跨这个暂停点存活
3. 将这些变量提升到 coroutine 结构体或保存在状态枚举中
4. 生成状态转换代码

对于嵌套的 await（无论是直接的还是函数调用嵌套的），编译器会：
- 为每个 await 分配一个状态
- 分析变量之间的依赖关系
- 确保所有需要的中间结果都被正确保存
- **不区分 await 的来源**（直接 Future、函数调用、嵌套函数调用）

### 关键变化
- 出现了**状态机枚举**（`State`），每个 `.await` 会增加一个状态变体。
- 需要跨 `.await` 存活的局部变量（`x`）被"捕获"到 `FooFuture` 结构体中。
- 代码被拆分成多个阶段，在不同的 `poll` 调用中执行。
- 如果有多个 `.await`，状态枚举会相应增加更多变体。

**关于变量 `y` 的详细说明**：

`y` 在这个例子中用于演示**跨暂停点存活的变量**（variables live across suspension points）的概念：

1. **`y` 的生命周期跨越了 `.await`**：
   - 在 `.await` **之前**：`y` 被计算为 `x + 2 = 1 + 2 = 3`
   - 在 `.await` **之后**：`y` 被用于计算 `result = y + 10 = 3 + 10 = 13`，然后作为返回值返回

2. **为什么 `y` 必须被提升到 `FooFuture` 结构体中**：
   - 当协程在 `.await` 处暂停（yield）时，函数栈会被销毁
   - 如果 `y` 只是普通的局部变量，它的值会在暂停时丢失
   - 因此编译器必须将 `y` "提升"（promote）到 `FooFuture` 结构体中，作为结构体的字段保存
   - 这样在协程恢复（resume）时，`y` 的值（3）才能被正确恢复，并用于：
      - 继续计算：`result = y + 10`
      - 作为返回值：`return result`

3. **`y` 的值在不同状态下的变化及其实际用途**：
   - **初始状态**：`y = 0`（在 `FooFuture { state: State::Unresumed, y: 0 }` 中初始化）
   - **State::Unresumed 执行后**：`y = x + 2 = 1 + 2 = 3`
   - **暂停时**：如果 `futures::future::ready(42).await` 返回 `Poll::Pending`，协程会暂停，此时 `y == 3` 被保存在 `FooFuture` 结构体中，状态变为 `State::Suspend0`（状态 3）
   - **恢复后**：当协程从 `State::Suspend0` 恢复时，`y` 的值仍然是 3（从结构体中恢复）
   - **最终状态**：`result = y + 10 = 3 + 10 = 13`
   - **`y` 的实际用途**：
      - 用于计算返回值：`result = y + 10`（使用保存的 y 值进行计算）
      - 作为返回值的一部分：`return result`（result 的值依赖于 y）

4. **如果 `y` 不被提升会发生什么**：
   - 如果 `y` 只是普通的局部变量，在协程暂停时，函数栈会被销毁
   - 当协程恢复时，`y` 的值会丢失，无法继续使用
   - 这会导致编译错误或运行时错误

5. **编译器如何自动识别需要提升的变量**：
   - 编译器会分析变量的生命周期，识别哪些变量在 `.await` 之前被使用，在 `.await` 之后继续被使用
   - 这些变量会被自动提升到 coroutine 结构体中
   - 只有真正需要跨暂停点存活的变量才会被提升，避免不必要的内存开销
   - **注意**：`x` 不需要提升，因为它在 `.await` 之前就完成了使用，不会在 `.await` 之后被使用

这个例子展示了 Rust 编译器如何自动处理跨暂停点的变量，开发者无需手动管理这些状态。

### 源代码实现

#### 什么是 Coroutine（协程）？

**简单理解**：Coroutine 就是一个**可以暂停和恢复的函数**。你可以把它想象成一个"书签"：执行到某个地方时可以暂停，保存当前状态，之后可以从暂停的地方继续执行。

**与 Future 的关系**：

在 `async/await` 的场景下，coroutine 主要用于配合 Future 工作：

- **Coroutine 是底层机制**：提供"暂停/恢复"的通用能力
- **Future 是上层应用**：利用 coroutine 实现"异步等待"的具体场景

**类比**：
- Coroutine = 通用的"暂停/恢复"机制（就像操作系统提供的线程切换能力）
- Future = 在异步场景下的具体应用（就像用线程切换实现异步 I/O）

**在 Rust 编译器中的角色**：

在 Rust 编译器中，**Coroutine（协程）** 是 `async fn`、`gen` 和 `async gen` 的底层抽象：

1. **`async fn`** → 转换为 coroutine → 实现 `Future` trait（异步场景）
2. **`gen fn`** → 转换为 coroutine → 可能实现 `Iterator` trait（生成器场景，实验性）
3. **`async gen`** → 转换为 coroutine → 可能实现 `AsyncIterator` trait（异步生成器，实验性）

**设计思路**：
- Coroutine 提供了一个统一的中间表示，可以表示多种控制流抽象
- 通过将 `async fn` 先转换为 coroutine，再在 MIR 阶段转换为状态机，编译器可以复用相同的转换逻辑
- Coroutine 的 `yield` 机制自然地对应了 `.await` 的暂停语义

**关键点**：
- 对于 `async fn`，编译器生成的 coroutine 类型**自动实现 `Future` trait**
- `Future::poll` 方法内部调用 `Coroutine::resume`，并将 `CoroutineState` 转换为 `Poll`
- 在 async/await 场景下，coroutine 就是 Future 的实现机制

#### Coroutine 相关的核心类型

Coroutine 相关的核心类型包括：

**标准库中的类型**（`library/core/src/ops/coroutine.rs`）：
- **`Coroutine<R>`** trait：协程的核心 trait，定义了 `resume` 方法和关联类型
- **`CoroutineState<Y, R>`** enum：协程恢复后的状态，包含 `Yielded(Y)` 和 `Complete(R)` 两个变体

**编译器 HIR 中的类型**（`compiler/rustc_hir/src/hir.rs`，注意：HIR 是编译器的内部数据结构，不是 Rust 代码）：
- **`CoroutineKind`** enum：区分不同类型的 coroutine（`Desugared` 或 `Coroutine`）
- **`CoroutineDesugaring`** enum：区分去糖类型（`Async`、`Gen`、`AsyncGen`）
- **`CoroutineSource`** enum：区分 coroutine 的来源（`Block`、`Closure`、`Fn`）

#### Coroutine Trait 定义

**文件**: `library/core/src/ops/coroutine.rs`（第 73-120 行）

```rust
pub trait Coroutine<R = ()> {
    /// The type of value this coroutine yields.
    #[lang = "coroutine_yield"]
    type Yield;

    /// The type of value this coroutine returns.
    #[lang = "coroutine_return"]
    type Return;

    /// Resumes the execution of this coroutine.
    #[lang = "coroutine_resume"]
    fn resume(self: Pin<&mut Self>, arg: R) -> CoroutineState<Self::Yield, Self::Return>;
}
```

#### CoroutineState 枚举

**文件**: `library/core/src/ops/coroutine.rs`（第 11-26 行）

```rust
pub enum CoroutineState<Y, R> {
    /// The coroutine suspended with a value.
    Yielded(Y),

    /// The coroutine completed with a return value.
    Complete(R),
}
```

#### Coroutine 与 Future 的关系

**关键转换**：在 MIR Transform 阶段，`StateTransform` pass 将 coroutine 转换为状态机，并实现 `Future::poll` 方法。

**文件**: `compiler/rustc_mir_transform/src/coroutine.rs`（第 1-51 行）

```rust
//! This is the implementation of the pass which transforms coroutines into state machines.
//!
//! This pass creates the implementation for either the `Coroutine::resume` or `Future::poll`
//! function and the drop shim for the coroutine based on the MIR input.
//! It computes the final layout of the coroutine struct which looks like this:
//!     First upvars are stored
//!     It is followed by the coroutine state field.
//!     Then finally the MIR locals which are live across a suspension point are stored.
//!     ```ignore (illustrative)
//!     struct Coroutine {
//!         upvars...,
//!         state: u32,
//!         mir_locals...,
//!     }
//!     ```
//! This pass computes the meaning of the state field and the MIR locals which are live
//! across a suspension point. There are however three hardcoded coroutine states:
//!     0 - Coroutine have not been resumed yet
//!     1 - Coroutine has returned / is completed
//!     2 - Coroutine has been poisoned
//!
//! It also rewrites `return x` and `yield y` as setting a new coroutine state and returning
//! `CoroutineState::Complete(x)` and `CoroutineState::Yielded(y)`,
//! or `Poll::Ready(x)` and `Poll::Pending` respectively.
```

**转换映射关系**：

编译器生成的 async coroutine 类型**自动实现 `Future` trait**。具体关系如下：

```rust
// 编译器生成的 async coroutine 类型（简化示意）
struct AsyncCoroutine {
    upvars: ...,
    state: u32,
    saved_locals: ...,
}

// 编译器自动为 async coroutine 实现 Future trait
impl Future for AsyncCoroutine {
    type Output = T;  // 原函数的返回类型
    
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        // 这个 poll 方法实际上就是 Coroutine::resume 的实现
        // 但返回类型从 CoroutineState 转换为 Poll
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
        // 注意：实际编译器使用 u32 类型的 discriminant，这里用 match 示意
        match self.state {
            // 状态 0：Unresumed（未开始）
            0 => {
                // 初始状态：执行代码
                // ... 执行代码 ...
                if 遇到 yield {
                    self.state = 暂停点编号;  // 设置为状态 3, 4, 5... 等
                    return CoroutineState::Yielded(());
                }
                self.state = 1;  // 状态 1：Returned（已完成）
                return CoroutineState::Complete(result);
            }
            // 状态 1：Returned（已完成）- 不应该到达这里，因为已完成
            1 => {
                panic!("future polled after completion");
            }
            // 状态 2：Panicked（已销毁）- 不应该到达这里
            2 => {
                panic!("future polled after panic");
            }
            // 状态 3+：Suspend0, Suspend1, ...（暂停点）
            n => {
                // 从暂停点 n 继续（n >= 3）
                // ... 继续执行 ...
                if 再次遇到 yield {
                    self.state = 新的暂停点;  // 设置为下一个暂停点编号
                    return CoroutineState::Yielded(());
                }
                self.state = 1;  // 状态 1：Returned（已完成）
                return CoroutineState::Complete(result);
            }
        }
    }
}
```

**关键转换点**：

1. **`Coroutine::resume` → `Future::poll`**：
   - 对于 async coroutine，`Future::poll` 内部调用 `Coroutine::resume`
   - `poll` 接收 `&mut Context<'_>`，转换为 `ResumeTy` 后传给 `resume`

2. **`CoroutineState::Yielded(_)` → `Poll::Pending`**：
   ```rust
   // 在 coroutine 内部
   yield ();  // 转换为：
   self.state = 暂停点;
   return CoroutineState::Yielded(());
   
   // 在 Future::poll 中
   match coroutine.resume(...) {
       CoroutineState::Yielded(_) => Poll::Pending,  // 转换
       CoroutineState::Complete(x) => Poll::Ready(x),
   }
   ```

3. **`CoroutineState::Complete(x)` → `Poll::Ready(x)`**：
   ```rust
   // 在 coroutine 内部
   return result;  // 转换为：
   self.state = Done;
   return CoroutineState::Complete(result);
   
   // 在 Future::poll 中
   match coroutine.resume(...) {
       CoroutineState::Complete(x) => Poll::Ready(x),  // 转换
       ...
   }
   ```

**总结**：async coroutine 类型同时实现了 `Coroutine` 和 `Future` 两个 trait，`Future::poll` 是对 `Coroutine::resume` 的包装，将 `CoroutineState` 转换为 `Poll`。

#### 为什么需要 Coroutine 而不是只用 Nested Future Poll？

理论上，我们可以通过嵌套的 Future poll 来实现类似功能，但 Rust 编译器选择了 Coroutine 作为底层抽象。原因如下：

##### 1. 统一的抽象层，支持多种控制流

Coroutine 不仅用于 `async fn`，还支持：
- **`async fn`** → 实现 `Future` trait
- **`gen fn`** → 实现 `Iterator` trait（实验性）
- **`async gen`** → 实现 `AsyncIterator` trait（实验性）
- **原生 coroutine** → 直接使用 `Coroutine` trait（实验性）

**文件**: `compiler/rustc_mir_transform/src/coroutine.rs`（第 1495-1521 行）

编译器复用同一套转换逻辑，根据 `CoroutineKind` 生成不同的返回类型：

```rust
let new_ret_ty = match coroutine_kind {
    CoroutineKind::Desugared(CoroutineDesugaring::Async, _) => {
        // Compute Poll<return_ty>
        Ty::new_adt(tcx, poll_adt_ref, poll_args)
    }
    CoroutineKind::Desugared(CoroutineDesugaring::Gen, _) => {
        // Compute Option<yield_ty>
        Ty::new_adt(tcx, option_adt_ref, option_args)
    }
    CoroutineKind::Desugared(CoroutineDesugaring::AsyncGen, _) => {
        // The yield ty is already `Poll<Option<yield_ty>>`
        old_yield_ty
    }
    CoroutineKind::Coroutine(_) => {
        // Compute CoroutineState<yield_ty, return_ty>
        Ty::new_adt(tcx, state_adt_ref, state_args)
    }
};
```

如果只用 nested future poll，需要为每种类型单独实现，代码重复且维护成本高。

##### 2. 更灵活的控制流表示

**Nested future poll 的局限性**：
- 只能表示"等待另一个 Future 完成"
- 难以优雅处理循环中的 await、条件分支、复杂的控制流

**Coroutine 的 `yield` 机制**：
- 可以表示任意暂停点，不限于等待 Future
- 支持循环、条件分支、递归等复杂控制流
- 状态机可以精确表示每个暂停点的上下文

例如，以下代码用 nested future poll 难以优雅处理：

```rust
async fn complex_flow() {
    for i in 0..10 {
        if condition().await {  // 循环中的 await
            // nested future poll 难以优雅处理这种场景
        }
    }
}
```

**如果用 nested future poll 手动实现**，代码会变得非常复杂：

```rust
// 手动实现：需要管理循环状态、迭代器状态、await 状态
// 注意：这里的 enum 只是手动实现的示意
// 实际编译器生成的是 u32 类型的 discriminant
enum ComplexFlowState {
    Unresumed,                          // 状态 0：未开始
    Returned,                           // 状态 1：已完成
    Panicked,                           // 状态 2：已销毁
    LoopInit,                           // 状态 3：循环初始化
    LoopIter { i: u32, condition_fut: ConditionFuture },  // 状态 4：循环迭代（等待 condition）
    LoopContinue { i: u32 },            // 状态 5：循环继续
}

struct ComplexFlowFuture {
    state: ComplexFlowState,
}

impl Future for ComplexFlowFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        loop {
            match &mut self.state {
                ComplexFlowState::Unresumed => {  // 状态 0：未开始
                    self.state = ComplexFlowState::LoopInit;
                }
                ComplexFlowState::LoopInit => {
                    self.state = ComplexFlowState::LoopIter {
                        i: 0,
                        condition_fut: condition(),
                    };
                }
                ComplexFlowState::LoopIter { i, condition_fut } => {
                    match Pin::new(condition_fut).poll(cx) {
                        Poll::Ready(true) => {
                            // condition 返回 true，继续循环
                            *i += 1;
                            if *i < 10 {
                                self.state = ComplexFlowState::LoopIter {
                                    i: *i,
                                    condition_fut: condition(),
                                };
                            } else {
                                self.state = ComplexFlowState::Returned;  // 状态 1：已完成
                                return Poll::Ready(());
                            }
                        }
                        Poll::Ready(false) => {
                            // condition 返回 false，继续循环
                            *i += 1;
                            if *i < 10 {
                                self.state = ComplexFlowState::LoopIter {
                                    i: *i,
                                    condition_fut: condition(),
                                };
                            } else {
                                self.state = ComplexFlowState::Returned;  // 状态 1：已完成
                                return Poll::Ready(());
                            }
                        }
                        Poll::Pending => {
                            // 需要保存当前状态，等待下次 poll
                            return Poll::Pending;
                        }
                    }
                }
                ComplexFlowState::LoopContinue { i } => {
                    // 从暂停点恢复，需要重新创建 condition_fut
                    self.state = ComplexFlowState::LoopIter {
                        i: *i,
                        condition_fut: condition(),
                    };
                }
                ComplexFlowState::Returned => {  // 状态 1：已完成
                    panic!("future polled after completion");
                }
                ComplexFlowState::Panicked => {  // 状态 2：已销毁
                    panic!("future polled after panic");
                }
            }
        }
    }
}
```

**问题**：
1. **状态管理复杂**：需要手动管理循环计数器 `i`、迭代器状态、await 状态
2. **代码重复**：每次循环都需要重复的状态转换逻辑
3. **容易出错**：手动管理状态容易遗漏边界情况（如循环结束条件）
4. **难以扩展**：如果循环中还有嵌套的 await，状态会指数级增长
5. **可读性差**：状态机代码与原始代码的对应关系不直观

**Coroutine 的实现方式**（编译器自动生成）：

编译器会将循环中的 await 转换为状态机，自动处理所有状态管理：

```rust
// 编译器生成的 coroutine 状态机（简化示意）
// 注意：实际编译器生成的是 u32 类型的 discriminant
enum ComplexFlowState {
    Unresumed,                          // 状态 0：未开始
    Returned,                           // 状态 1：已完成
    Panicked,                           // 状态 2：已销毁
    Suspend0 { i: u32 },                // 状态 3：第 0 次循环的暂停点，等待 condition
    Suspend1 { i: u32 },                // 状态 4：第 1 次循环的暂停点，等待 condition
    // ... 为每次循环的 await 生成一个状态
    Suspend9 { i: u32 },                // 状态 12：第 9 次循环的暂停点，等待 condition
}

impl Coroutine<ResumeTy> for ComplexFlowCoroutine {
    fn resume(mut self: Pin<&mut Self>, arg: ResumeTy) -> CoroutineState<(), ()> {
        let cx = unsafe { get_context(arg) };
        match self.state {
            ComplexFlowState::Unresumed => {  // 状态 0：未开始
                // 编译器自动展开循环
                let mut i = 0;
                while i < 10 {
                    // 遇到 await，保存状态并 yield
                    let mut condition_fut = condition();
                    match Pin::new(&mut condition_fut).poll(cx) {
                        Poll::Ready(_) => {
                            // 继续循环
                            i += 1;
                        }
                        Poll::Pending => {
                            // 保存循环状态，yield
                            self.state = ComplexFlowState::Suspend0 { i };  // 状态 3
                            return CoroutineState::Yielded(());
                        }
                    }
                }
                self.state = ComplexFlowState::Returned;  // 状态 1：已完成
                CoroutineState::Complete(())
            }
            ComplexFlowState::Suspend0 { i } => {  // 状态 3：第一个暂停点
                // 从暂停点恢复，继续循环
                let mut condition_fut = condition();
                match Pin::new(&mut condition_fut).poll(cx) {
                    Poll::Ready(_) => {
                        let mut i = *i;
                        i += 1;
                        // 继续循环...
                    }
                    Poll::Pending => {
                        return CoroutineState::Yielded(());
                    }
                }
                // ...
            }
            // ...
        }
    }
}
```

**Coroutine 的优势**：
1. **自动状态管理**：编译器自动识别需要保存的变量（如循环计数器 `i`）
2. **自动状态生成**：为每个暂停点自动生成对应的状态变体
3. **代码简洁**：原始代码逻辑清晰，编译器负责转换
4. **易于扩展**：嵌套的 await 和复杂的控制流都能自动处理
5. **可读性强**：生成的代码与原始代码的对应关系清晰

**实际编译器优化**：编译器会进行优化，不会为每次循环都生成一个状态，而是复用状态并保存循环计数器。但核心思想是：编译器自动处理所有状态管理，开发者无需关心这些细节。

##### 3. 统一的状态管理和变量提升

Coroutine 提供了统一的状态管理机制：
- 自动识别跨暂停点存活的局部变量
- 将这些变量提升到 coroutine 结构体中
- 生成状态枚举，每个暂停点对应一个状态

**文件**: `compiler/rustc_mir_transform/src/coroutine.rs`（第 1-51 行）

编译器会自动：
1. 分析哪些局部变量需要跨暂停点存活
2. 将这些变量提升到 coroutine 结构体中
3. 生成状态枚举，每个暂停点对应一个状态变体

如果只用 nested future poll，需要手动管理这些状态，容易出错且代码复杂。

##### 4. 统一的 Drop 处理

Coroutine 提供了统一的 drop 处理机制：
- 正确处理暂停点的资源清理
- 区分未开始、进行中、已完成、已销毁等状态
- 支持同步和异步 drop

**文件**: `compiler/rustc_mir_transform/src/coroutine.rs`（第 1655-1675 行）

编译器会生成 `coroutine_drop` 和 `coroutine_drop_async` 两个函数：

```rust
if has_async_drops {
    // 生成异步 drop shim
    let mut drop_shim = create_coroutine_drop_shim_async(...);
    body.coroutine.as_mut().unwrap().coroutine_drop_async = Some(drop_shim);
} else {
    // 生成同步 drop shim
    let mut drop_shim = create_coroutine_drop_shim(...);
    body.coroutine.as_mut().unwrap().coroutine_drop = Some(drop_shim);
}
```

根据 coroutine 的状态（未开始、进行中、已完成、已销毁）进行正确的资源清理。

##### 5. 编译器的实现优势

从 `compiler/rustc_mir_transform/src/coroutine.rs` 的实现来看：
- 同一个 `StateTransform` pass 处理所有类型的 coroutine
- 同一个 `create_coroutine_resume_function` 生成 resume/poll 函数
- 同一个变量提升和状态机生成逻辑

**文件**: `compiler/rustc_mir_transform/src/coroutine.rs`（第 1466-1682 行）

如果只用 nested future poll，需要为每种场景单独实现，维护成本高。

##### 6. 语义清晰度

**Coroutine 的语义更清晰**：
- `yield` 明确表示"暂停并返回控制权"
- `resume` 明确表示"从暂停点继续执行"
- `CoroutineState` 明确表示协程的状态

**Nested future poll 的语义相对模糊**：
- 只是"调用另一个 Future 的 poll"
- 没有明确的"暂停/恢复"概念
- 状态管理需要手动处理

##### 总结

虽然 nested future poll 理论上可以实现类似功能，但 Coroutine 提供了：
1. **统一的抽象层**，支持多种控制流模式（async、gen、async gen）
2. **更灵活的控制流表示能力**，可以处理复杂的控制流
3. **统一的状态管理和变量提升**，自动处理跨暂停点的变量
4. **统一的 drop 处理机制**，正确处理资源清理
5. **代码复用和维护优势**，编译器可以复用相同的转换逻辑
6. **更清晰的语义**，明确表示暂停/恢复的概念

这些优势使得 Coroutine 成为 Rust 编译器实现 `async/await`、`gen`、`async gen` 等特性的统一底层机制，而不是为每种特性单独实现一套逻辑。

**Coroutine 状态机布局**（简化示意）：
```rust
struct Coroutine {
    // 1. 捕获的变量（upvars）
    upvars: ...,
    
    // 2. 状态字段（discriminant）
    state: u32,  // 0 = 未开始, 1 = 完成, 2 = 已销毁, 3+ = 暂停点
    
    // 3. 跨暂停点存活的局部变量
    mir_locals: ...,
}
```

#### 1. `.await` 表达式的转换（HIR 阶段）

`.await` 在 AST Lowering 阶段被转换为一个 `loop` + `match` + `yield` 的模式。相关代码位于 `compiler/rustc_ast_lowering/src/expr.rs`（第 850-1049 行）。

关键转换逻辑（第 900-1049 行）：

```rust
        let expr_hir_id = expr.hir_id;

        // Note that the name of this binding must not be changed to something else because
        // debuggers and debugger extensions expect it to be called `__awaitee`. They use
        // this name to identify what is being awaited by a suspended async functions.
        let awaitee_ident = Ident::with_dummy_span(sym::__awaitee);
        let (awaitee_pat, awaitee_pat_hid) =
            self.pat_ident_binding_mode(gen_future_span, awaitee_ident, hir::BindingMode::MUT);

        let task_context_ident = Ident::with_dummy_span(sym::_task_context);

        // unsafe {
        //     ::std::future::Future::poll(
        //         ::std::pin::Pin::new_unchecked(&mut __awaitee),
        //         ::std::future::get_context(task_context),
        //     )
        // }
        let poll_expr = {
            let awaitee = self.expr_ident(span, awaitee_ident, awaitee_pat_hid);
            let ref_mut_awaitee = self.expr_mut_addr_of(span, awaitee);

            let Some(task_context_hid) = self.task_context else {
                unreachable!("use of `await` outside of an async context.");
            };

            let task_context = self.expr_ident_mut(span, task_context_ident, task_context_hid);

            let new_unchecked = self.expr_call_lang_item_fn_mut(
                span,
                hir::LangItem::PinNewUnchecked,
                arena_vec![self; ref_mut_awaitee],
            );
            let get_context = self.expr_call_lang_item_fn_mut(
                gen_future_span,
                hir::LangItem::GetContext,
                arena_vec![self; task_context],
            );
            let call = match await_kind {
                FutureKind::Future => self.expr_call_lang_item_fn(
                    span,
                    hir::LangItem::FuturePoll,
                    arena_vec![self; new_unchecked, get_context],
                ),
                FutureKind::AsyncIterator => self.expr_call_lang_item_fn(
                    span,
                    hir::LangItem::AsyncIteratorPollNext,
                    arena_vec![self; new_unchecked, get_context],
                ),
            };
            self.arena.alloc(self.expr_unsafe(span, call))
        };

        // `::std::task::Poll::Ready(result) => break result`
        let loop_node_id = self.next_node_id();
        let loop_hir_id = self.lower_node_id(loop_node_id);
        let ready_arm = {
            let x_ident = Ident::with_dummy_span(sym::result);
            let (x_pat, x_pat_hid) = self.pat_ident(gen_future_span, x_ident);
            let x_expr = self.expr_ident(gen_future_span, x_ident, x_pat_hid);
            let ready_field = self.single_pat_field(gen_future_span, x_pat);
            let ready_pat = self.pat_lang_item_variant(span, hir::LangItem::PollReady, ready_field);
            let break_x = self.with_loop_scope(loop_hir_id, move |this| {
                let expr_break =
                    hir::ExprKind::Break(this.lower_loop_destination(None), Some(x_expr));
                this.arena.alloc(this.expr(gen_future_span, expr_break))
            });
            self.arm(ready_pat, break_x)
        };

        // `::std::task::Poll::Pending => {}`
        let pending_arm = {
            let pending_pat = self.pat_lang_item_variant(span, hir::LangItem::PollPending, &[]);
            let empty_block = self.expr_block_empty(span);
            self.arm(pending_pat, empty_block)
        };

        let inner_match_stmt = {
            let match_expr = self.expr_match(
                span,
                poll_expr,
                arena_vec![self; ready_arm, pending_arm],
                hir::MatchSource::AwaitDesugar,
            );
            self.stmt_expr(span, match_expr)
        };

        // Depending on `async` of `async gen`:
        // async     - task_context = yield ();
        // async gen - task_context = yield ASYNC_GEN_PENDING;
        let yield_stmt = {
            let yielded = if is_async_gen {
                self.arena.alloc(self.expr_lang_item_path(span, hir::LangItem::AsyncGenPending))
            } else {
                self.expr_unit(span)
            };

            let yield_expr = self.expr(
                span,
                hir::ExprKind::Yield(yielded, hir::YieldSource::Await { expr: Some(expr_hir_id) }),
            );
            let yield_expr = self.arena.alloc(yield_expr);

            let Some(task_context_hid) = self.task_context else {
                unreachable!("use of `await` outside of an async context.");
            };

            let lhs = self.expr_ident(span, task_context_ident, task_context_hid);
            let assign =
                self.expr(span, hir::ExprKind::Assign(lhs, yield_expr, self.lower_span(span)));
            self.stmt_expr(span, assign)
        };

        let loop_block = self.block_all(span, arena_vec![self; inner_match_stmt, yield_stmt], None);

        // loop { .. }
        let loop_expr = self.arena.alloc(hir::Expr {
            hir_id: loop_hir_id,
            kind: hir::ExprKind::Loop(
                loop_block,
                None,
                hir::LoopSource::Loop,
                self.lower_span(span),
            ),
            span: self.lower_span(span),
        });

        // mut __awaitee => loop { ... }
        let awaitee_arm = self.arm(awaitee_pat, loop_expr);

        // `match ::std::future::IntoFuture::into_future(<expr>) { ... }`
        let into_future_expr = match await_kind {
            FutureKind::Future => self.expr_call_lang_item_fn(
                span,
                hir::LangItem::IntoFutureIntoFuture,
                arena_vec![self; *expr],
            ),
            // Not needed for `for await` because we expect to have already called
            // `IntoAsyncIterator::into_async_iter` on it.
            FutureKind::AsyncIterator => expr,
        };

        // match <into_future_expr> {
        //     mut __awaitee => loop { .. }
        // }
        hir::ExprKind::Match(
            into_future_expr,
            arena_vec![self; awaitee_arm],
            hir::MatchSource::AwaitDesugar,
        )
```

转换后的结构大致为：
```rust
match IntoFuture::into_future(expr) {
    mut __awaitee => loop {
        match unsafe { 
            Future::poll(
                Pin::new_unchecked(&mut __awaitee),
                get_context(_task_context)
            ) 
        } {
            Poll::Ready(result) => break result,
            Poll::Pending => {
                _task_context = yield ();
                // 继续循环
            }
        }
    }
}
```

`get_context` 函数定义在 `library/core/src/future/mod.rs`（第 59-68 行）：

```rust
#[lang = "get_context"]
#[doc(hidden)]
#[unstable(feature = "gen_future", issue = "none")]
#[must_use]
#[inline]
pub unsafe fn get_context<'a, 'b>(cx: ResumeTy) -> &'a mut Context<'b> {
    // SAFETY: the caller must guarantee that `cx.0` is a valid pointer
    // that fulfills all the requirements for a mutable reference.
    unsafe { &mut *cx.0.as_ptr().cast() }
}
```

#### 2. 状态机转换（MIR 阶段）

在 MIR Transform 阶段，`StateTransform` pass 将 coroutine 转换为状态机。相关代码位于 `compiler/rustc_mir_transform/src/coroutine.rs`（第 1466-1682 行）。

关键转换步骤（第 1495-1565 行）：

```rust
        let new_ret_ty = match coroutine_kind {
            CoroutineKind::Desugared(CoroutineDesugaring::Async, _) => {
                // Compute Poll<return_ty>
                let poll_did = tcx.require_lang_item(LangItem::Poll, body.span);
                let poll_adt_ref = tcx.adt_def(poll_did);
                let poll_args = tcx.mk_args(&[old_ret_ty.into()]);
                Ty::new_adt(tcx, poll_adt_ref, poll_args)
            }
            CoroutineKind::Desugared(CoroutineDesugaring::Gen, _) => {
                // Compute Option<yield_ty>
                let option_did = tcx.require_lang_item(LangItem::Option, body.span);
                let option_adt_ref = tcx.adt_def(option_did);
                let option_args = tcx.mk_args(&[old_yield_ty.into()]);
                Ty::new_adt(tcx, option_adt_ref, option_args)
            }
            CoroutineKind::Desugared(CoroutineDesugaring::AsyncGen, _) => {
                // The yield ty is already `Poll<Option<yield_ty>>`
                old_yield_ty
            }
            CoroutineKind::Coroutine(_) => {
                // Compute CoroutineState<yield_ty, return_ty>
                let state_did = tcx.require_lang_item(LangItem::CoroutineState, body.span);
                let state_adt_ref = tcx.adt_def(state_did);
                let state_args = tcx.mk_args(&[old_yield_ty.into(), old_ret_ty.into()]);
                Ty::new_adt(tcx, state_adt_ref, state_args)
            }
        };

        // We need to insert clean drop for unresumed state and perform drop elaboration
        // (finally in open_drop_for_tuple) before async drop expansion.
        // Async drops, produced by this drop elaboration, will be expanded,
        // and corresponding futures kept in layout.
        let has_async_drops = matches!(
            coroutine_kind,
            CoroutineKind::Desugared(CoroutineDesugaring::Async | CoroutineDesugaring::AsyncGen, _)
        ) && has_expandable_async_drops(tcx, body, coroutine_ty);

        // Replace all occurrences of `ResumeTy` with `&mut Context<'_>` within async bodies.
        if matches!(
            coroutine_kind,
            CoroutineKind::Desugared(CoroutineDesugaring::Async | CoroutineDesugaring::AsyncGen, _)
        ) {
            let context_mut_ref = transform_async_context(tcx, body);
            expand_async_drops(tcx, body, context_mut_ref, coroutine_kind, coroutine_ty);

            if let Some(dumper) = MirDumper::new(tcx, "coroutine_async_drop_expand", body) {
                dumper.dump_mir(body);
            }
        } else {
            cleanup_async_drops(body);
        }

        let always_live_locals = always_storage_live_locals(body);
        let movable = coroutine_kind.movability() == hir::Movability::Movable;
        let liveness_info =
            locals_live_across_suspend_points(tcx, body, &always_live_locals, movable);

        if tcx.sess.opts.unstable_opts.validate_mir {
            let mut vis = EnsureCoroutineFieldAssignmentsNeverAlias {
                assigned_local: None,
                saved_locals: &liveness_info.saved_locals,
                storage_conflicts: &liveness_info.storage_conflicts,
            };

            vis.visit_body(body);
        }

        // Extract locals which are live across suspension point into `layout`
        // `remap` gives a mapping from local indices onto coroutine struct indices
        // `storage_liveness` tells us which locals have live storage at suspension points
        let (remap, layout, storage_liveness) = compute_layout(liveness_info, body);
```

**转换后的结构**：这个阶段将 coroutine 转换为状态机，主要步骤包括：

1. **返回类型转换**：
   - `async fn` → 返回类型从 `T` 变为 `Poll<T>`
   - `gen fn` → 返回类型变为 `Option<Yield>`
   - 普通 coroutine → 返回类型变为 `CoroutineState<Yield, Return>`

2. **ResumeTy 替换**：在 async 函数体中，所有 `ResumeTy` 被替换为 `&mut Context<'_>`

3. **局部变量分析**：
   - 计算哪些局部变量在暂停点之间存活（`locals_live_across_suspend_points`）
   - 这些变量需要保存在状态机结构体中

4. **布局计算**（`compute_layout`）：
   - 生成状态机结构体布局：`{ upvars, state, saved_locals }`
   - 创建从 local 索引到状态机字段的映射（`remap`）

转换过程会：
1. 计算需要跨暂停点保存的局部变量（`locals_live_across_suspend_points`）
2. 计算 coroutine 布局（`compute_layout`），将 locals 映射到状态机结构体的字段
3. 转换 MIR，将 locals 访问改为状态机字段访问，将 `yield` 和 `return` 转换为状态设置和 `Poll` 返回

```rust
// 文件: compiler/rustc_mir_transform/src/coroutine.rs（第 1574-1591 行）
        // Run the transformation which converts Places from Local to coroutine struct
        // accesses for locals in `remap`.
        // It also rewrites `return x` and `yield y` as writing a new coroutine state and returning
        // either `CoroutineState::Complete(x)` and `CoroutineState::Yielded(y)`,
        // or `Poll::Ready(x)` and `Poll::Pending` respectively depending on the coroutine kind.
        let mut transform = TransformVisitor {
            tcx,
            coroutine_kind,
            remap,
            storage_liveness,
            always_live_locals,
            suspension_points: Vec::new(),
            discr_ty,
            new_ret_local,
            old_ret_ty,
            old_yield_ty,
        };
        transform.visit_body(body);
```

**转换后的结构**：`TransformVisitor` 遍历 MIR 并执行以下转换：

1. **局部变量访问转换**：
   ```rust
   // 转换前：访问局部变量
   _1 = 42;
   
   // 转换后：访问状态机字段
   self.field_0 = 42;  // field_0 对应原来的 _1
   ```

2. **yield 表达式转换**（对于 async）：
   ```rust
   // 转换前：yield ()
   yield ();
   
   // 转换后：设置状态 + 返回 Poll::Pending
   self.state = SuspensionPoint1;
   return Poll::Pending;
   ```

3. **return 表达式转换**（对于 async）：
   ```rust
   // 转换前：return x
   return x;
   
   // 转换后：设置完成状态 + 返回 Poll::Ready
   self.state = Done;
   return Poll::Ready(x);
   ```

最后，创建 `Future::poll` 实现（即 `Coroutine::resume` 函数）：

```rust
// 文件: compiler/rustc_mir_transform/src/coroutine.rs（第 1677-1678 行）
        // Create the Coroutine::resume / Future::poll function
        create_coroutine_resume_function(tcx, transform, body, can_return, can_unwind);
```

**转换后的结构**：`create_coroutine_resume_function` 创建 `Future::poll` 方法，大致结构为：

```rust
impl Future for Coroutine {
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Output> {
        match self.state {
            0 => {
                // 未开始：执行初始代码
                // ... 初始代码 ...
                if 遇到 yield {
                    self.state = 暂停点编号;
                    return Poll::Pending;
                }
                self.state = 1;  // Done
                return Poll::Ready(result);
            }
            1 => {
                // 已完成：panic
                panic!("future polled after completion");
            }
            2 => {
                // 已销毁：panic
                panic!("future polled after drop");
            }
            n => {
                // 暂停点 n：从该点继续执行
                // ... 从暂停点继续的代码 ...
                if 再次遇到 yield {
                    self.state = 新的暂停点编号;
                    return Poll::Pending;
                }
                self.state = 1;  // Done
                return Poll::Ready(result);
            }
        }
    }
}
```

## 总结对比表

| 代码特点                           | 是否生成状态机 | Future 大小      | 执行时机                              | 典型开销          |
|------------------------------------|----------------|------------------|---------------------------------------|-------------------|
| 无 `.await`，只有同步代码           | 否             | 几乎 0（ZST）    | 第一次 `poll` 时同步执行               | 几乎为零          |
| 包含一个或多个 `.await`             | 是             | 有状态（enum + 字段） | 分段执行，依赖多次 `poll` 和 wake     | 状态机内存开销    |

## 核心结论

Rust 的 `async`/`await` 是真正的**零成本抽象**：
- 当你不需要异步暂停时，它退化为几乎无开销的同步代码。
- 只有真正需要暂停（`.await`）时，编译器才会生成必要的状态机来保存上下文。

这正是 Rust 异步模型强大而高效的原因：你只为真正使用的功能付出代价。

（本文基于 Rust 编译器当前（截至 2025 年底）的去糖行为撰写，实际生成的代码会经过更多优化，但核心原理一致。）


---

Rust 的 `async fn` 和 `.await` 在编译时**不会直接展开成用户可见的 Rust Future 代码**（即不是源代码级的手动 impl Future），而是经过一系列内部表示的转换，最终生成高效的状态机。

### 具体过程（简要编译管道）：

1. **源代码 → AST → HIR**（High-level Intermediate Representation）：  
   `async fn` 和 `.await` 在 AST Lowering 阶段被**去糖（desugar）** 为 **coroutine**（协程）的形式。
   - **重要说明**：HIR **不是 Rust 代码**，而是编译器的**内部数据结构**（Rust 结构体和枚举的集合）。虽然 HIR 保留了 Rust 的语法结构，但它已经是编译器的内存表示，不是源代码。虽然可以用 `-Z unpretty=hir` 输出类似 Rust 的文本，但那只是格式化输出，用于调试和查看。实际的 HIR 是内存中的数据结构。
   - **文件**: `compiler/rustc_ast_lowering/src/item.rs:1573-1593`
   - `async` 函数被转换为 coroutine 闭包表达式，返回类型变为 `impl Future<Output = T>`（通过 opaque type 实现）。
   - `.await` 被转换为 `loop { match poll(...) { Ready => break, Pending => yield } }` 模式。
   - **文件**: `compiler/rustc_ast_lowering/src/expr.rs:850-1049`
   - 对于 async 函数，会添加 `ResumeTy` 参数和 `_task_context` 变量（`compiler/rustc_ast_lowering/src/expr.rs:720-751`）。

2. **HIR → THIR → MIR**（Mid-level Intermediate Representation）：  
   coroutine 在 MIR Transform 阶段被进一步转换为**状态机**：
   - **文件**: `compiler/rustc_mir_transform/src/coroutine.rs:1466-1682`
   - `StateTransform` pass 执行转换：
      - 计算需要跨暂停点保存的局部变量（`locals_live_across_suspend_points`）
      - 计算状态机布局（`compute_layout`），将 locals 映射到状态机结构体的字段
      - 将 locals 访问改为状态机字段访问
      - 将 `yield` 转换为状态设置 + `Poll::Pending` 返回
      - 将 `return` 转换为状态设置 + `Poll::Ready` 返回
      - 创建 `Future::poll` 实现（`create_coroutine_resume_function`）
   - 一个枚举表示不同暂停点（每个 `.await` 对应一个状态变体）。
   - 跨 `.await` 的局部变量被提升到状态机结构体中。
   - `poll` 方法实现为 switch/jump table，根据当前状态执行对应代码段。  
     无 `.await` 时，状态机可能被优化为几乎零开销的立即完成 Future。

3. **MIR → LLVM IR → 机器码**：  
   MIR 经过借用检查、优化、monomorphization 等后交给 LLVM 生成最终代码。

**结论**：
- 不是"展开为 Rust 的 Future 代码"（源代码级），而是直接在 **MIR** 层生成状态机（基于 coroutine 转换）。
- 你可以用 `cargo rustc -- --emit mir` 查看生成的 MIR，能看到状态机枚举和 poll 逻辑（但很底层）。
- 关键 lang items：
   - `ResumeTy`: `library/core/src/future/mod.rs:47-57`
   - `get_context`: `library/core/src/future/mod.rs:59-68`
   - `CoroutineState`: `library/core/src/ops/coroutine.rs:11-26`

### MIR 和 HIR 的区别与关系

**重要说明**：HIR 和 MIR 都是编译器的**内部数据结构**，不是 Rust 代码。它们都是内存中的数据结构（Rust 结构体和枚举的集合），虽然可以用工具输出类似 Rust 的文本，但那只是格式化输出，用于调试和查看。

| 方面         | HIR (High-level IR)                          | MIR (Mid-level IR)                           |
|--------------|----------------------------------------------|----------------------------------------------|
| **本质**     | 编译器内部数据结构（结构体/枚举），不是代码 | 编译器内部数据结构（结构体/枚举），不是代码 |
| **抽象级别** | 高：接近表面语法，保留 match、for、async 等构造 | 中：大幅简化，无 match、for、async 等高级构造 |
| **用途**     | 类型检查、名称解析、宏扩展后的大部分分析     | 借用检查（borrow checker）、优化、代码生成   |
| **特点**     | 仍包含生命周期、复杂表达式、desugar 不彻底   | 明确控制流（基本块 + terminator）、显式借用、适合流敏感分析 |
| **关系**     | 从 AST lowering 而来，async/await 在此初步 desugar | 从 HIR（经 THIR）构建而来，async/await 在此彻底转为状态机 |
| **查看方式** | `rustc --pretty=hir`（格式化输出，非实际 HIR） | `rustc --emit mir` 或 playground 的 MIR 按钮（格式化输出，非实际 MIR） |

**总结**：HIR 是"带糖的抽象语法树"（数据结构），用于早期编译阶段；MIR 是"核心 Rust"的简化形式（数据结构），用于关键安全检查和优化。async/await 的核心魔法发生在从 HIR 到 MIR 的转换中。

## 补充：Coroutine 类型系统

在 HIR 中（注意：HIR 是编译器的内部数据结构，不是 Rust 代码），coroutine 的类型信息通过以下枚举表示：

### CoroutineKind

**文件**: `compiler/rustc_hir/src/hir.rs`（第 2166-2208 行）

```rust
pub enum CoroutineKind {
    /// A coroutine that comes from a desugaring.
    Desugared(CoroutineDesugaring, CoroutineSource),

    /// A coroutine literal created via a `yield` inside a closure.
    Coroutine(Movability),
}

impl CoroutineKind {
    pub fn movability(self) -> Movability {
        match self {
            CoroutineKind::Desugared(CoroutineDesugaring::Async, _)
            | CoroutineKind::Desugared(CoroutineDesugaring::AsyncGen, _) => Movability::Static,
            CoroutineKind::Desugared(CoroutineDesugaring::Gen, _) => Movability::Movable,
            CoroutineKind::Coroutine(mov) => mov,
        }
    }

    pub fn is_fn_like(self) -> bool {
        matches!(self, CoroutineKind::Desugared(_, CoroutineSource::Fn))
    }

    pub fn to_plural_string(&self) -> String {
        match self {
            CoroutineKind::Desugared(d, CoroutineSource::Fn) => format!("{d:#}fn bodies"),
            CoroutineKind::Desugared(d, CoroutineSource::Block) => format!("{d:#}blocks"),
            CoroutineKind::Desugared(d, CoroutineSource::Closure) => format!("{d:#}closure bodies"),
            CoroutineKind::Coroutine(_) => "coroutines".to_string(),
        }
    }
}

impl fmt::Display for CoroutineKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoroutineKind::Desugared(d, k) => {
                d.fmt(f)?;
                k.fmt(f)
            }
            CoroutineKind::Coroutine(_) => f.write_str("coroutine"),
        }
    }
}
```

### CoroutineDesugaring

**文件**: `compiler/rustc_hir/src/hir.rs`（第 2239-2279 行）

```rust
pub enum CoroutineDesugaring {
    /// An explicit `async` block or the body of an `async` function.
    Async,

    /// An explicit `gen` block or the body of a `gen` function.
    Gen,

    /// An explicit `async gen` block or the body of an `async gen` function,
    /// which is able to both `yield` and `.await`.
    AsyncGen,
}

impl fmt::Display for CoroutineDesugaring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoroutineDesugaring::Async => {
                if f.alternate() {
                    f.write_str("`async` ")?;
                } else {
                    f.write_str("async ")?
                }
            }
            CoroutineDesugaring::Gen => {
                if f.alternate() {
                    f.write_str("`gen` ")?;
                } else {
                    f.write_str("gen ")?
                }
            }
            CoroutineDesugaring::AsyncGen => {
                if f.alternate() {
                    f.write_str("`async gen` ")?;
                } else {
                    f.write_str("async gen ")?
                }
            }
        }

        Ok(())
    }
}
```

### CoroutineSource

**文件**: `compiler/rustc_hir/src/hir.rs`（第 2214-2236 行）

```rust
/// In the case of a coroutine created as part of an async/gen construct,
/// this indicates what kind of construct it was created from.
/// This is needed for error messages and for some special handling during
/// type-checking (see #60424).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Copy, HashStable_Generic, Encodable, Decodable)]
pub enum CoroutineSource {
    /// An explicit `async`/`gen` block written by the user.
    Block,

    /// An explicit `async`/`gen` closure written by the user.
    Closure,

    /// The `async`/`gen` block generated as the body of an async/gen function.
    Fn,
}
```

这些类型信息在编译器的各个阶段用于区分不同类型的 coroutine，并影响代码生成和优化策略。

## 补充：MIR 和 HIR 的设计与关系

### 是否是 Rust 特有的设计？

**HIR（High-level Intermediate Representation）** 和 **MIR（Mid-level Intermediate Representation）** 是 Rust 编译器特有的中间表示设计。虽然"中间表示"（IR）是编译器的通用概念，但 HIR 和 MIR 的具体设计、结构和用途都是为 Rust 语言特性量身定制的。

**与其他编译器的对比**：
- **LLVM IR**：通用的中间表示，被多种语言（C/C++、Rust、Swift 等）使用
- **HIR/MIR**：Rust 专用，针对 Rust 的所有权、生命周期、借用检查等特性设计

### 编译管道中的位置

Rust 编译器的完整 IR 转换流程：

```
源代码 (Rust)
  ↓
Token Stream (词法分析)
  ↓
AST (语法分析)
  ↓
HIR (AST Lowering + 宏展开 + 名称解析)
  ↓
THIR (Typed HIR，类型检查后的 HIR)
  ↓
MIR (Mid-level IR，基于控制流图)
  ↓
LLVM IR (代码生成)
  ↓
机器码
```

### HIR 的特点与用途

**文件**: `compiler/rustc_hir/src/hir.rs`

**重要澄清**：HIR **不是 Rust 代码**，而是编译器的**内部数据结构**（Rust 结构体和枚举的集合）。虽然 HIR 保留了 Rust 的语法结构，但它已经是编译器的内存表示，不是源代码。

**特点**：
- **数据结构表示**：HIR 是 Rust 结构体/枚举的集合，存储在内存中（使用 arena 分配器）
- **高抽象级别**：结构上接近源代码语法，保留 `match`、`async` 等高级构造
- **去糖不彻底**：仍包含生命周期、复杂表达式、部分语法糖
- **树形结构**：类似 AST，但经过宏展开和名称解析

**主要用途**：
- 类型检查（type checking）
- 名称解析（name resolution）
- 宏展开后的分析
- Trait 求解（trait solving）

**示例**：在 HIR 中，`for` 循环已被转换为 `loop`，但 `async fn` 仍保留为 coroutine 表达式。

**查看 HIR**：虽然可以用 `-Z unpretty=hir` 输出类似 Rust 的文本，但那只是格式化输出，用于调试和查看。实际的 HIR 是内存中的数据结构。

### MIR 的特点与用途

**文件**: `compiler/rustc_middle/src/mir/mod.rs`

**特点**：
- **基于控制流图（CFG）**：使用基本块（basic blocks）和边（edges）表示程序
- **无嵌套表达式**：所有表达式都被展平为语句序列
- **显式类型**：所有类型信息完全显式
- **简化表示**：无 `match`、`for`、`async` 等高级构造，只有基本操作

**主要用途**：
- **借用检查（borrow checking）**：MIR 的流敏感特性使其非常适合借用检查
- **数据流分析**：检查未初始化值、死代码等
- **优化**：在泛型代码上进行优化，比 monomorphization 后更高效
- **常量求值**：通过 MIRI 进行编译时求值

**MIR 的关键概念**（`compiler/rustc_middle/src/mir/mod.rs`）：
- **Basic Block**：基本块，包含语句序列和终止符
- **Statement**：语句，只有一个后继（如赋值）
- **Terminator**：终止符，可能有多个后继（如分支、调用）
- **Local**：局部变量，用索引表示（如 `_1`、`_2`）
- **Place**：内存位置表达式（如 `_1`、`_1.f`）
- **Rvalue**：产生值的表达式（如 `_1 + _2`）

### HIR 到 MIR 的转换关系

**转换过程**（`compiler/rustc_mir_build/src/builder/mod.rs`）：

1. **HIR → THIR**：
   - 进行类型检查，所有类型信息完全确定
   - 方法调用和隐式解引用被显式化
   - 为 MIR 构建做准备

2. **THIR → MIR**：
   - 递归处理 THIR 表达式
   - 将高级构造（如 `match`、`if`）转换为基本块和跳转
   - 创建局部变量和临时变量
   - 生成控制流图

**转换示例**：

```rust
// HIR 数据结构表示（简化，实际是内存中的结构体/枚举）
// 对应源代码：match x { Some(v) => v + 1, None => 0 }
// HIR 中存储为 ExprKind::Match { ... } 等数据结构

// 转换为 MIR 数据结构（简化示意）
// MIR 中存储为基本块和控制流图
bb0: {
    _2 = discriminant(_1);  // 检查枚举判别式
    switchInt(_2) -> [0: bb1, 1: bb2];  // 分支
}
bb1: {  // None 分支
    _0 = const 0;
    goto -> bb3;
}
bb2: {  // Some 分支
    _3 = ((_1 as Some).0: i32);  // 解构
    _0 = Add(_3, const 1);
    goto -> bb3;
}
bb3: {
    return;
}
```

**注意**：上面的代码示例只是为了展示转换逻辑，实际的 HIR 和 MIR 都是内存中的数据结构（结构体、枚举等），不是文本代码。`-Z unpretty=hir` 和 `--emit mir` 只是将这些数据结构格式化为可读的文本形式。

### 为什么需要两个 IR？

**设计原因**：

1. **关注点分离**：
   - **HIR**：适合高级分析（类型检查、名称解析），保留源代码结构
   - **MIR**：适合流敏感分析（借用检查、数据流），简化控制流

2. **性能考虑**：
   - HIR 保留源代码结构，便于增量编译
   - MIR 的 CFG 结构便于数据流分析和优化

3. **Rust 特性支持**：
   - HIR 保留生命周期信息，便于类型检查
   - MIR 的显式借用和流敏感特性，便于借用检查

### 在 async/await 中的作用

在 async/await 的编译过程中：

1. **HIR 阶段**：coroutine 实现机制 + Future 类型系统抽象（两者并存但分离）
   - **实现机制**：`async fn` 被转换为 coroutine 闭包表达式
      - 接收 `ResumeTy` 参数（coroutine 的特征）
      - 包含 `yield ()` 暂停点（coroutine 的核心机制）
   - **类型系统**：返回类型是 `impl Future<Output = T>`（opaque type with Future bound）
   - `.await` 被转换为 `loop { match poll(...) { Ready => break, Pending => yield } }` 模式
   - 仍保留高级语法结构

2. **MIR Transform 阶段**：将 coroutine 和 Future 统一为状态机 + `Future::poll` 实现
   - **状态机生成**：基于 coroutine 机制生成状态机结构体
      - 状态字段（discriminant）的生成
      - 状态变体（variant）的布局计算
      - 变量提升（locals_live_across_suspend_points）
   - **函数生成**：根据 coroutine 类型选择生成 `Future::poll` 或 `Coroutine::resume`
      - 对于 async coroutine：生成 `Future::poll`（返回 `Poll<T>`）
      - 对于 gen coroutine：生成 `Coroutine::resume`（返回 `CoroutineState<Y, R>`）
   - **转换映射**：
      - `yield ()` → 状态设置 + `Poll::Pending`（对于 async）
      - `return x` → 状态设置 + `Poll::Ready(x)`（对于 async）
   - 生成基于基本块的控制流图
   - 进行借用检查和优化

**关键理解**：
- **不是"混合"**，而是"统一"：HIR 阶段 coroutine 和 Future 是分离的（实现机制 vs 类型系统），MIR Transform 阶段将它们统一为状态机 + `Future::poll` 实现
- **Coroutine 是底层机制**：提供"暂停/恢复"的能力，状态机的生成、变量提升等都基于 coroutine 机制
- **Future 是上层抽象**：类型系统层面的抽象，在 MIR 阶段通过生成 `Future::poll` 实现来体现

**关键文件**：
- HIR 构建：`compiler/rustc_ast_lowering/src/`
- MIR 构建：`compiler/rustc_mir_build/src/`
- MIR 转换：`compiler/rustc_mir_transform/src/`

### 查看 HIR 和 MIR

**查看 HIR**：
```bash
cargo rustc -- -Z unpretty=hir-tree  # 树形结构
cargo rustc -- -Z unpretty=hir       # 更接近源代码
```

**查看 MIR**：
```bash
cargo rustc -- --emit mir            # 输出 MIR
cargo rustc -- -Z mir-opt-level=0 --emit mir  # 未优化的 MIR
```

也可以在 [Rust Playground](https://play.rust-lang.org/) 中点击 "MIR" 按钮查看。

### 总结
z
HIR 和 MIR 是 Rust 编译器特有的设计，它们的分层架构使得编译器能够：
- 在 HIR 层面进行高级分析和类型检查
- 在 MIR 层面进行流敏感的安全检查和优化
- 为 Rust 的所有权系统、借用检查等特性提供合适的分析基础

这种设计是 Rust 编译器能够高效、准确地分析和编译 Rust 代码的关键基础。