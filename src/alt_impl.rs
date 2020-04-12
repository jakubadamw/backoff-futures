use backoff::backoff::Backoff;
use backoff::Error;
use std::future::Future;
use std::time::Duration;

struct BackoffFutureBuilder<'b, B, F, Fut, T, E>
where
    B: Backoff,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, Error<E>>>,
{
    backoff: &'b mut B,
    f: F,
}

impl<'b, B, F, Fut, T, E> BackoffFutureBuilder<'b, B, F, Fut, T, E>
where
    B: Backoff,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, Error<E>>>,
{
    async fn fut<N: FnMut(&Error<E>, Duration)>(mut self, mut notify: N) -> Result<T, Error<E>> {
        loop {
            let work_result = (self.f)().await;
            match work_result {
                Ok(_) | Err(Error::Permanent(_)) => return work_result,
                Err(Error::Transient(e)) => {
                    let err = Error::Transient(e);

                    if let Some(backoff_duration) = self.backoff.next_backoff() {
                        notify(&err, backoff_duration);
                        tokio::time::delay_for(backoff_duration).await
                    } else {
                        return Err(err);
                    }
                }
            }
        }
    }
}

pub async fn with_backoff<B, F, Fut, T, E>(f: F, backoff: &mut B) -> Result<T, Error<E>>
where
    B: Backoff,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, Error<E>>>,
{
    let backoff_struct = BackoffFutureBuilder { backoff, f };
    backoff_struct.fut(|_, _| {}).await
}

pub async fn with_backoff_notify<B, F, Fut, T, E, N>(
    f: F,
    backoff: &mut B,
    notify: N,
) -> Result<T, Error<E>>
where
    B: Backoff,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, Error<E>>>,
    N: FnMut(&Error<E>, Duration),
{
    let backoff_struct = BackoffFutureBuilder { backoff, f };
    backoff_struct.fut(notify).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::Future;

    #[test]
    fn test_when_future_succeeds() {
        fn do_work() -> impl Future<Output = Result<u32, backoff::Error<()>>> {
            futures::future::ready(Ok(123))
        }

        let mut backoff = backoff::ExponentialBackoff::default();
        let result: Result<u32, backoff::Error<()>> =
            futures::executor::block_on(with_backoff(do_work, &mut backoff));
        assert_eq!(result.ok(), Some(123));
    }

    #[test]
    fn test_with_closure_when_future_succeeds() {
        let do_work = || futures::future::lazy(|_| Ok(123));

        let mut backoff = backoff::ExponentialBackoff::default();
        let result: Result<u32, backoff::Error<()>> =
            futures::executor::block_on(with_backoff(do_work, &mut backoff));
        assert_eq!(result.ok(), Some(123));
    }

    #[test]
    fn test_with_closure_when_future_fails_with_permanent_error() {
        let do_work = || {
            let result = Err(backoff::Error::Permanent(()));
            futures::future::ready(result)
        };

        let mut backoff = backoff::ExponentialBackoff::default();
        let result: Result<u32, backoff::Error<()>> =
            futures::executor::block_on(with_backoff(do_work, &mut backoff));
        assert_matches!(result.err(), Some(backoff::Error::Permanent(_)));
    }

    #[test]
    fn test_with_async_fn_when_future_succeeds() {
        async fn do_work() -> Result<u32, backoff::Error<()>> {
            Ok(123)
        }

        let mut backoff = backoff::ExponentialBackoff::default();
        let result: Result<u32, backoff::Error<()>> =
            futures::executor::block_on(with_backoff(do_work, &mut backoff));
        assert_eq!(result.ok(), Some(123));
    }

    #[test]
    fn test_with_async_fn_when_future_fails_for_some_time() {
        let mut runtime = tokio::runtime::Runtime::new().expect("tokio runtime creation");
        static mut CALL_COUNTER: usize = 0;
        const CALLS_TO_SUCCESS: usize = 5;
        async fn do_work() -> Result<u32, backoff::Error<()>> {
            unsafe {
                CALL_COUNTER += 1;
                if CALL_COUNTER != CALLS_TO_SUCCESS {
                    Err(backoff::Error::Transient(()))
                } else {
                    Ok(123)
                }
            }
        };

        let mut backoff = backoff::ExponentialBackoff::default();
        backoff.current_interval = Duration::from_millis(1);
        backoff.initial_interval = Duration::from_millis(1);

        let mut notify_counter = 0;

        let result = runtime.block_on(with_backoff_notify(do_work, &mut backoff, |e, d| {
            notify_counter += 1;
            println!("Error {:?}, waiting for: {}", e, d.as_millis());
        }));

        unsafe {
            assert_eq!(CALL_COUNTER, CALLS_TO_SUCCESS);
        }
        assert_eq!(CALLS_TO_SUCCESS, notify_counter + 1);
        assert_eq!(result.ok(), Some(123));
    }
}
