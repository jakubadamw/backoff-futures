#![feature(async_await)]

#[cfg(test)] #[macro_use] extern crate matches;

use std::pin::Pin;
use std::task::{Context, Poll};

use backoff::backoff::{Backoff};
use futures::Future;

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

pub trait TryFutureExt<Fut, B, F> {
    fn with_backoff(self, backoff: &mut B) -> BackoffFuture<'_, Fut, B, F>;
}

impl<Fut, T, E, B, F> TryFutureExt<Fut, B, F> for F
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
    where Fut: futures::Future<Output = Result<T, backoff::Error<E>>>,
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
    use super::TryFutureExt;

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
