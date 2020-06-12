//! An add-on to [`std::future::Future`] that makes it easy to introduce a retry mechanism
//! with a backoff for functions that produce failible futures,
//! i.e. futures where the `Output` type is some `Result<T, backoff::Error<E>>`.
//! The `backoff::Error` wrapper is necessary so as to distinguish errors that are considered
//! *transient*, and thus make it likely that a future attempt at producing and blocking on
//! the same future could just as well succeed (e.g. the HTTP 503 Service Unavailable error),
//! and errors that are considered *permanent*, where no future attempts are presumed to have
//! a chance to succeed (e.g. the HTTP 404 Not Found error).
//!
//! The extension trait integrates with the `backoff` crate and expects a [`backoff::backoff::Backoff`]
//! value to describe the various properties of the retry & backoff mechanism to be used.
//!
//! ```rust
//! fn isahc_error_to_backoff(err: isahc::Error) -> backoff::Error<isahc::Error> {
//!     match err {
//!         isahc::Error::Aborted | isahc::Error::Io(_) | isahc::Error::Timeout =>
//!             backoff::Error::Transient(err),
//!         _ =>
//!             backoff::Error::Permanent(err)
//!     }
//! }
//!
//! async fn get_example_contents() -> Result<String, backoff::Error<isahc::Error>> {
//!     use isahc::ResponseExt;
//!
//!     let mut response = isahc::get_async("https://example.org")
//!         .await
//!         .map_err(isahc_error_to_backoff)?;
//!
//!     response
//!         .text_async()
//!         .await
//!         .map_err(|err: std::io::Error| backoff::Error::Transient(isahc::Error::Io(err)))
//! }
//!
//! async fn get_example_contents_with_retry() -> Result<String, isahc::Error> {
//!     use backoff_futures::BackoffExt;
//!
//!     let mut backoff = backoff::ExponentialBackoff::default();
//!     get_example_contents.with_backoff(&mut backoff)
//!         .await
//!         .map_err(|err| match err {
//!             backoff::Error::Transient(err) | backoff::Error::Permanent(err) => err
//!         })
//! }
//! ```
//!
//! See [`BackoffExt::with_backoff`] for more details.

#![allow(clippy::type_repetition_in_bounds)]

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
                Err(err @ Error::Transient(_)) => {
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

#[async_trait::async_trait(?Send)]
pub trait BackoffExt<T, E, Fut, F> {
    /// Returns a future that, when polled, will first ask `self` for a new future (with an output
    /// type `Result<T, backoff::Error<_>>` to produce the expected result.
    ///
    /// If the underlying future is ready with an `Err` value, the nature of the error
    /// (permanent/transient) will determine whether polling the future will employ the provided
    /// `backoff` strategy and will result in the work being retried.
    ///
    /// Specifically, [`backoff::Error::Permanent`] errors will be returned immediately.
    /// [`backoff::Error::Transient`] errors will, depending on the particular [`backoff::backoff::Backoff`],
    /// result in a retry attempt, most likely with a delay.
    ///
    /// If the underlying future is ready with an [`std::result::Result::Ok`] value, it will be returned immediately.
    async fn with_backoff<B>(self, backoff: &mut B) -> Result<T, Error<E>>
    where
        B: Backoff,
        T: 'async_trait,
        E: 'async_trait,
        Fut: 'async_trait;
    
    /// Same as [`BackoffExt::with_backoff`] but takes an extra `notify` closure that will be called every time
    /// a new backoff is employed on transient errors. The closure takes the new delay duration as an argument.
    async fn with_backoff_notify<B, N>(self, backoff: &mut B, notify: N) -> Result<T, Error<E>>
    where
        B: Backoff,
        N: FnMut(&Error<E>, Duration),
        T: 'async_trait,
        E: 'async_trait,
        Fut: 'async_trait;
}

#[async_trait::async_trait(?Send)]
impl<T, E, Fut, F> BackoffExt<T, E, Fut, F> for F
     where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, backoff::Error<E>>> {

    async fn with_backoff<B>(self, backoff: &mut B) -> Result<T, Error<E>>
    where
        B: Backoff,
        T: 'async_trait,
        E: 'async_trait,
        Fut: 'async_trait
    {
        let backoff_struct = BackoffFutureBuilder { backoff, f: self };
        backoff_struct.fut(|_, _| {}).await
    }

    async fn with_backoff_notify<B, N>(self, backoff: &mut B, notify: N) -> Result<T, Error<E>>
    where
        B: Backoff,
        N: FnMut(&Error<E>, Duration),
        T: 'async_trait,
        E: 'async_trait,
        Fut: 'async_trait
    {
        let backoff_struct = BackoffFutureBuilder { backoff, f: self };
        backoff_struct.fut(notify).await
    }
}

#[cfg(test)]
mod tests {
    use super::BackoffExt;
    use futures::Future;

    #[test]
    fn test_when_future_succeeds() {
        fn do_work() -> impl Future<Output = Result<u32, backoff::Error<()>>> {
            futures::future::ready(Ok(123))
        }

        let mut backoff = backoff::ExponentialBackoff::default();
        let result: Result<u32, backoff::Error<()>> =
            futures::executor::block_on(do_work.with_backoff(&mut backoff));
        assert_eq!(result.ok(), Some(123));
    }

    #[test]
    fn test_with_closure_when_future_succeeds() {
        let do_work = || {
            futures::future::lazy(|_| Ok(123))
        };

        let mut backoff = backoff::ExponentialBackoff::default();
        let result: Result<u32, backoff::Error<()>> =
            futures::executor::block_on(do_work.with_backoff(&mut backoff));
        assert_eq!(result.ok(), Some(123));
    }

    #[test]
    fn test_with_closure_when_future_fails_with_permanent_error() {
        use matches::assert_matches;

        let do_work = || {
            let result = Err(backoff::Error::Permanent(()));
            futures::future::ready(result)
        };

        let mut backoff = backoff::ExponentialBackoff::default();
        let result: Result<u32, backoff::Error<()>> =
            futures::executor::block_on(do_work.with_backoff(&mut backoff));
        assert_matches!(result.err(), Some(backoff::Error::Permanent(_)));
    }

    #[test]
    fn test_with_async_fn_when_future_succeeds() {
        async fn do_work() -> Result<u32, backoff::Error<()>> {
            Ok(123)
        }

        let mut backoff = backoff::ExponentialBackoff::default();
        let result: Result<u32, backoff::Error<()>> =
            futures::executor::block_on(do_work.with_backoff(&mut backoff));
        assert_eq!(result.ok(), Some(123));
    }

    #[test]
    fn test_with_async_fn_when_future_fails_for_some_time() {
        static mut CALL_COUNTER: usize = 0;
        const CALLS_TO_SUCCESS: usize = 5;

        use std::time::Duration;

        async fn do_work() -> Result<u32, backoff::Error<()>> {
            unsafe {
                CALL_COUNTER += 1;
                if CALL_COUNTER == CALLS_TO_SUCCESS {
                    Ok(123)
                } else {
                    Err(backoff::Error::Transient(()))
                }
            }
        };

        let mut backoff = backoff::ExponentialBackoff::default();
        backoff.current_interval = Duration::from_millis(1);
        backoff.initial_interval = Duration::from_millis(1);

        let mut notify_counter = 0;

        let mut runtime = tokio::runtime::Runtime::new()
            .expect("tokio runtime creation");

        let result = runtime.block_on(do_work.with_backoff_notify(&mut backoff, |e, d| {
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
