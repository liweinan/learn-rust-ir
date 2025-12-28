use futures::executor::block_on;

async fn foo() -> i32 {
    let x = 1;
    let y = x + 2;
    
    // 引入一个 .await 来观察状态机的生成
    futures::future::ready(42).await;
    
    let result = y + 10;
    result
}

fn main() {
    let result = block_on(foo());
    println!("Result: {}", result);
}


