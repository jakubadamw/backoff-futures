# backoff-futures

### DEPRECATED: see `backoff::future`.

[![crates.io](https://img.shields.io/crates/v/backoff-futures.svg)](https://crates.io/crates/backoff-futures)

A retry and backoff mechanism for `std::future::Future`.

[Documentation](https://docs.rs/backoff-futures/latest/backoff_futures/)

## Adding as a dependency

### Manually

```toml
[dependencies]
backoff-futures = "0.3"
```

## Usage

```rust
#![feature(async_await)]

fn isahc_error_to_backoff(err: isahc::Error) -> backoff::Error<isahc::Error> {
    match err {
        isahc::Error::Aborted | isahc::Error::Io(_) | isahc::Error::Timeout =>
            backoff::Error::Transient(err),
        _ =>
            backoff::Error::Permanent(err)
    }
}

async fn get_example_contents() -> Result<String, backoff::Error<isahc::Error>> {
    use isahc::ResponseExt;

    let mut response = isahc::get_async("https://example.org")
        .await
        .map_err(isahc_error_to_backoff)?;

    response
        .text_async()
        .await
        .map_err(|err: std::io::Error| backoff::Error::Transient(isahc::Error::Io(err)))
}

async fn get_example_contents_with_retry() -> Result<String, isahc::Error> {
    use backoff_futures::BackoffExt;

    let mut backoff = backoff::ExponentialBackoff::default();
    get_example_contents.with_backoff(&mut backoff)
        .await
        .map_err(|err| match err {
            backoff::Error::Transient(err) | backoff::Error::Permanent(err) => err
        })
}
```

## License
[MIT](https://choosealicense.com/licenses/mit/)

