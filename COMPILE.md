# Rust async/await 编译器内部机制详解

## 前言

Rust 的 `async`/`await` 语法看起来非常简洁线性，但实际上在编译阶段会被彻底"去糖"（desugar）为一个手写状态机式的 `Future`。本文基于 Rust 编译器源代码，通过逐步添加代码的例子，展示编译器在不同情况下的行为差异，并提供具体的源代码位置和关键代码片段。

## 情况一：async fn 中没有 .await（只有同步代码）

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

    enum State {
        Start,
        Done,
    }

    impl Future for FooFuture {
        type Output = i32;
        fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<i32> {
            match self.state {
                State::Start => {
                    // 所有同步代码在第一次 poll 时执行
                    let x = 1;
                    let result = x;  // 这里执行计算，结果在运行时确定
                    self.state = State::Done;
                    Poll::Ready(result)
                }
                State::Done => panic!("future polled after completion"),
            }
        }
    }

    FooFuture { state: State::Start }
}
```

**注意**：即使没有 `.await`，编译器仍会生成一个简单的状态机（只有 Start 和 Done 两个状态）。如果函数体包含复杂计算，这些计算会在第一次 `poll` 时执行，结果在运行时确定，而不是编译时。

### 特点
- 即使没有 `.await`，编译器仍会生成一个简单的状态机（只有 Start 和 Done 两个状态）。
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
async fn foo() {
    let mut x = 1;
    x = x + 1;                  // 同步操作，x == 2
    some_other_future().await;  // 暂停点
    x = x + 10;                 // 必须等到 .await 完成后才能执行
    println!("x = {}", x);      // 输出 x = 12
}
```

### 编译后大致等价于（手动去糖）

```rust
enum State {
    Start,
    Awaiting(SomeOtherFuture),
    Done,
}

struct FooFuture {
    state: State,
    x: i32,  // 需要跨 .await 存活的变量被提升到这里
}

impl Future for FooFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        loop {
            match self.state {
                State::Start => {
                    self.x = 1;
                    self.x += 1;  // x == 2

                    let mut sub = some_other_future();
                    match Pin::new(&mut sub).poll(cx) {
                        Poll::Ready(()) => {},  // 立即完成，继续往下执行
                        Poll::Pending => {
                            self.state = State::Awaiting(sub);
                            return Poll::Pending;
                        }
                    }

                    // 如果子 Future 立即完成，直接执行后面的代码
                    self.x += 10;
                    println!("x = {}", self.x);
                    self.state = State::Done;
                    return Poll::Ready(());
                }

                State::Awaiting(ref mut sub) => {
                    match Pin::new(sub).poll(cx) {
                        Poll::Ready(()) => {
                            self.x += 10;  // 从之前的 2 变成 12
                            println!("x = {}", self.x);
                            self.state = State::Done;
                            return Poll::Ready(());
                        }
                        Poll::Pending => return Poll::Pending,
                    }
                }

                State::Done => panic!("future polled after completion"),
            }
        }
    }
}

fn foo() -> FooFuture {
    FooFuture { state: State::Start, x: 0 }
}
```

### 关键变化
- 出现了**状态机枚举**（`State`），每个 `.await` 会增加一个状态变体。
- 需要跨 `.await` 存活的局部变量（`x`）被"捕获"到 `FooFuture` 结构体中。
- 代码被拆分成多个阶段，在不同的 `poll` 调用中执行。
- 如果有多个 `.await`，状态枚举会相应增加更多变体。

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

1. **HIR 阶段**：
    - `async fn` 被转换为 coroutine 表达式
    - `.await` 被转换为 `loop { match poll(...) { ... } }` 模式
    - 仍保留高级语法结构

2. **MIR 阶段**：
    - coroutine 被转换为状态机
    - `yield` 和 `return` 被转换为状态设置和 `Poll` 返回
    - 生成基于基本块的控制流图
    - 进行借用检查和优化

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

HIR 和 MIR 是 Rust 编译器特有的设计，它们的分层架构使得编译器能够：
- 在 HIR 层面进行高级分析和类型检查
- 在 MIR 层面进行流敏感的安全检查和优化
- 为 Rust 的所有权系统、借用检查等特性提供合适的分析基础

这种设计是 Rust 编译器能够高效、准确地分析和编译 Rust 代码的关键基础。