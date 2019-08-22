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

#[cfg(test)] #[macro_use] extern crate matches;

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use backoff::backoff::{Backoff};

enum BackoffState<Fut> {
    Pending,
    Delay(tokio_timer::Delay),
    Work(Fut)
}

#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct BackoffFuture<'b, Fut, B, F> {
    state: BackoffState<Fut>,
    backoff: &'b mut B,
    f: F
}

pub trait BackoffExt<Fut, B, F> {
    /// Returns a future that, when polled, will first ask `self` for a new future (with an output
    /// type `Result<T, backoff::Error<_>>` to produce the expected result.
    ///
    /// If the underlying future is ready with an `Err` value, the nature of the error
    /// (permanent/transient) will determine whether polling the future will employ the provided
    /// `backoff` strategy and will result in the the work being retried.
    ///
    /// Specifically, `backoff::Error::Permanent` errors will be returned immediately.
    /// [`backoff::Error::Transient`] errors will, depending on the particular [`backoff::backoff::Backoff`],
    /// result in a retry attempt, most likely with a delay.
    ///
    /// If the underlying future is ready with an `Ok` value, it will be returned immediately.
    fn with_backoff(self, backoff: &mut B) -> BackoffFuture<'_, Fut, B, F>;
}

impl<Fut, T, E, B, F> BackoffExt<Fut, B, F> for F
     where F: FnMut() -> Fut,
           Fut: Future<Output = Result<T, backoff::Error<E>>> {
    fn with_backoff(self, backoff: &mut B) -> BackoffFuture<'_, Fut, B, F> {
        BackoffFuture {
            f: self,
            state: BackoffState::Pending,
            backoff
        }
    }
}

impl<Fut, F, B, T, E> Future for BackoffFuture<'_, Fut, B, F>
    where Fut: Future<Output = Result<T, backoff::Error<E>>>,
          F: FnMut() -> Fut + Unpin,
          B: Backoff + Unpin
{
    type Output = Fut::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        use tokio_timer::Delay;
        use std::time::Instant;

        // The loop will be passed at most twice.
        loop {
            match self.as_mut().state {
                BackoffState::Work(_) => {
                    let fut = unsafe {
                        self.as_mut().map_unchecked_mut(|s| match s.state {
                            BackoffState::Work(ref mut f) => f,
                            _ => unreachable!()
                        })
                    };
        
                    match fut.poll(cx) {
                        Poll::Pending => return Poll::Pending,

                        Poll::Ready(value) => match value {
                            Ok(_) =>
                                return Poll::Ready(value),

                            Err(backoff::Error::Permanent(_)) =>
                                return Poll::Ready(value),

                            Err(backoff::Error::Transient(_)) => unsafe {
                                let mut s = self.as_mut().get_unchecked_mut();
                                match s.backoff.next_backoff() {
                                    Some(next) => {
                                        let delay = Delay::new(Instant::now() + next);
                                        s.state = BackoffState::Delay(delay);
                                    }
                                    None =>
                                        return Poll::Ready(value)
                                }
                            }
                        }
                    }
                }

                BackoffState::Delay(ref delay) if !delay.is_elapsed() =>
                    return Poll::Pending,

                _ => unsafe {
                    let mut s = self.as_mut().get_unchecked_mut();
                    s.state = BackoffState::Work((s.f)());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::Future;
    use super::BackoffExt;

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
            futures::future::ready(Ok(123)).await
        }

        let mut backoff = backoff::ExponentialBackoff::default();
        let result: Result<u32, backoff::Error<()>> =
            futures::executor::block_on(do_work.with_backoff(&mut backoff));
        assert_eq!(result.ok(), Some(123));
    }
}
