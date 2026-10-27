//! Upload body that hands its bytes to the connection in small chunks through the bandwidth
//! limiter, so the limit holds on the wire instead of only on average per part.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, ready};

use aws_sdk_s3::primitives::ByteStream;
use aws_smithy_types::body::SdkBody;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use tokio::time::Sleep;

use crate::limit::RateLimiter;

/// Bytes released per limiter request.
const CHUNK: usize = 64 * 1024;

/// Retryable body for `data`. `streamed` counts the bytes handed out by bodies that are still
/// alive; each body takes its share back when dropped, so a retried part is not counted twice.
pub(crate) fn body(data: Bytes, limiter: Arc<RateLimiter>, streamed: Arc<AtomicU64>) -> ByteStream {
    ByteStream::new(SdkBody::retryable(move || {
        SdkBody::from_body_1_x(Throttled {
            data: data.clone(),
            limiter: limiter.clone(),
            streamed: streamed.clone(),
            handed_out: 0,
            sleep: None,
        })
    }))
}

struct Throttled {
    data: Bytes,
    limiter: Arc<RateLimiter>,
    streamed: Arc<AtomicU64>,
    handed_out: u64,
    sleep: Option<Pin<Box<Sleep>>>,
}

impl Body for Throttled {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let this = &mut *self;
        loop {
            if let Some(sleep) = this.sleep.as_mut() {
                ready!(sleep.as_mut().poll(cx));
                this.sleep = None;
            }
            if this.data.is_empty() {
                return Poll::Ready(None);
            }
            let n = this.data.len().min(CHUNK);
            match this.limiter.reserve(n as u64) {
                Some(wait) => this.sleep = Some(Box::pin(tokio::time::sleep(wait))),
                None => {
                    let chunk = this.data.split_to(n);
                    this.handed_out += n as u64;
                    this.streamed.fetch_add(n as u64, Ordering::Relaxed);
                    return Poll::Ready(Some(Ok(Frame::data(chunk))));
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.data.is_empty()
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.data.len() as u64)
    }
}

impl Drop for Throttled {
    fn drop(&mut self) {
        self.streamed.fetch_sub(self.handed_out, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use http_body_util::BodyExt;
    use tokio::time::Instant;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn releases_chunks_at_the_limit() {
        // 8 chunks of 64 KiB at 128 KiB/s: the first goes at once, each next one waits 0.5 s.
        let limiter = Arc::new(RateLimiter::new(128 * 1024));
        let streamed = Arc::new(AtomicU64::new(0));
        let mut body = Throttled {
            data: Bytes::from(vec![7u8; 512 * 1024]),
            limiter,
            streamed: streamed.clone(),
            handed_out: 0,
            sleep: None,
        };
        assert_eq!(body.size_hint().exact(), Some(512 * 1024));
        let start = Instant::now();
        let mut total = 0;
        while let Some(frame) = body.frame().await {
            total += frame.unwrap().into_data().unwrap().len();
            assert_eq!(streamed.load(Ordering::Relaxed), total as u64);
        }
        assert_eq!(total, 512 * 1024);
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(3400), "{elapsed:?}");
        assert!(elapsed <= Duration::from_millis(3600), "{elapsed:?}");
        drop(body);
        assert_eq!(streamed.load(Ordering::Relaxed), 0);
    }
}
