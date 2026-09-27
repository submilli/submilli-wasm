use std::future::{poll_fn, Future};
use std::task::{Context, Poll, Waker};

use submilli_wasm::{Config, Engine, Func, FuncType, Instance, Store, Val, ValType};

pub fn run() {
    let mut config = Config::new();
    config.async_support(true);
    let engine = Engine::new(&config).unwrap();
    let mut store = Store::new(&engine, ());
    let host = Func::new_async(
        &mut store,
        FuncType::new(&engine, [], [ValType::I32]),
        |_, _, results| {
            Box::new(async move {
                let mut yielded = false;
                poll_fn(|cx| {
                    if yielded {
                        Poll::Ready(())
                    } else {
                        yielded = true;
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                })
                .await;
                results[0] = Val::I32(42);
                Ok(())
            })
        },
    );
    let guest = super::module(
        &engine,
        r#"(module
        (import "host" "value" (func $value (result i32)))
        (func (export "run") (result i32) call $value))"#,
    );
    let instance = ready(Instance::new_async(&mut store, &guest, &[host.into()]));
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    let mut future = Box::pin(run.call_async(&mut store, ()));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(future.as_mut().poll(&mut cx).is_pending());
    assert_eq!(ready(future), 42);
}

fn ready<T>(future: impl Future<Output = submilli_wasm::Result<T>>) -> T {
    let mut future = Box::pin(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(result) => result.unwrap(),
        Poll::Pending => panic!("expected completion"),
    }
}
