//! Bounded diagnostic copies; the original HTTP frames remain the transport.
use axum::body::{Body, Bytes, HttpBody};
use hyper::body::{Frame, SizeHint};
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

#[derive(Default)]
pub(super) struct CapturedBody {
    pub bytes: Vec<u8>,
    pub observed_bytes: u64,
    pub complete: bool,
    pub failed: bool,
    pub truncated: bool,
}

impl CapturedBody {
    pub fn is_complete(&self) -> bool {
        self.complete && !self.failed && !self.truncated
    }
    pub fn marker(&self, label: &str) -> String {
        let reason = if self.failed {
            "body stream failed"
        } else if self.truncated {
            "diagnostic limit reached"
        } else {
            "body was not fully consumed"
        };
        format!("[{label} diagnostic omitted: {reason}; {} bytes observed, {} bytes retained; transport unchanged]", self.observed_bytes, self.bytes.len())
    }
}

struct CaptureState {
    captured: CapturedBody,
    limit: usize,
    stopped: bool,
}

#[derive(Clone)]
pub(super) struct CaptureHandle(Arc<Mutex<CaptureState>>);

impl CaptureHandle {
    /// Freeze the diagnostic snapshot. Further transport reads still pass through.
    pub fn take(&self) -> CapturedBody {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        std::mem::take(&mut state.captured)
    }
}

type OnFinish = Box<dyn FnOnce(CapturedBody) + Send>;

#[cfg(test)]
fn capture_body(body: Body, limit: usize, on_finish: Option<OnFinish>) -> (Body, CaptureHandle) {
    capture_body_inner(body, limit, on_finish, None)
}

#[derive(Clone)]
struct BodyScope {
    capture: crate::proxy::monitor::UpstreamRequestBodyHolder,
    budget: Option<Arc<crate::proxy::pipeline::budget::RequestContext>>,
}

impl BodyScope {
    fn run<R>(&self, work: impl FnOnce() -> R) -> R {
        crate::proxy::pipeline::budget::sync_scope(self.budget.clone(), || {
            crate::proxy::monitor::CURRENT_UPSTREAM_CAPTURE.sync_scope(self.capture.clone(), work)
        })
    }
}

pub(super) fn capture_body_in_scope(
    body: Body,
    limit: usize,
    on_finish: Option<OnFinish>,
    capture: crate::proxy::monitor::UpstreamRequestBodyHolder,
    budget: Option<Arc<crate::proxy::pipeline::budget::RequestContext>>,
) -> (Body, CaptureHandle) {
    capture_body_inner(body, limit, on_finish, Some(BodyScope { capture, budget }))
}

fn capture_body_inner(
    body: Body,
    limit: usize,
    on_finish: Option<OnFinish>,
    scope: Option<BodyScope>,
) -> (Body, CaptureHandle) {
    let handle = CaptureHandle(Arc::new(Mutex::new(CaptureState {
        captured: CapturedBody {
            complete: body.is_end_stream(),
            ..Default::default()
        },
        limit,
        stopped: false,
    })));
    (
        Body::new(DiagnosticBody {
            inner: body,
            capture: handle.clone(),
            on_finish,
            scope,
        }),
        handle,
    )
}

struct DiagnosticBody {
    inner: Body,
    capture: CaptureHandle,
    on_finish: Option<OnFinish>,
    scope: Option<BodyScope>,
}

impl DiagnosticBody {
    fn finish(&mut self) {
        if let Some(callback) = self.on_finish.take() {
            callback(self.capture.take());
        }
    }

    fn finish_transport(&mut self) {
        let finish = |body: &mut Self| {
            let inner = std::mem::replace(&mut body.inner, Body::empty());
            drop(inner);
            body.finish();
        };
        if let Some(scope) = self.scope.clone() {
            scope.run(|| finish(self));
        } else {
            finish(self);
        }
    }
}

impl Drop for DiagnosticBody {
    fn drop(&mut self) {
        // Drop lazy upstream work before observing its final accounting.
        self.finish_transport();
    }
}

impl HttpBody for DiagnosticBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let scope = self.scope.clone();
        let result = if let Some(scope) = scope {
            scope.run(|| Pin::new(&mut self.inner).poll_frame(context))
        } else {
            Pin::new(&mut self.inner).poll_frame(context)
        };
        if let Poll::Ready(frame) = &result {
            let mut state = self.capture.0.lock().unwrap_or_else(|e| e.into_inner());
            if !state.stopped {
                match frame {
                    Some(Ok(frame)) => {
                        if let Some(bytes) = frame.data_ref() {
                            state.captured.observed_bytes = state
                                .captured
                                .observed_bytes
                                .saturating_add(bytes.len() as u64);
                            let remaining = state.limit.saturating_sub(state.captured.bytes.len());
                            state
                                .captured
                                .bytes
                                .extend_from_slice(&bytes[..remaining.min(bytes.len())]);
                            state.captured.truncated |= bytes.len() > remaining;
                        }
                    }
                    Some(Err(_)) => state.captured.failed = true,
                    None => state.captured.complete = true,
                }
                state.captured.complete |= self.inner.is_end_stream();
            }
            let finished = frame.is_none() || self.inner.is_end_stream();
            drop(state);
            if finished {
                self.finish_transport();
            }
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct Frames {
        frames: VecDeque<Result<Frame<Bytes>, std::io::Error>>,
        polls: Arc<AtomicUsize>,
    }
    impl HttpBody for Frames {
        type Data = Bytes;
        type Error = std::io::Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(self.frames.pop_front())
        }
    }

    #[tokio::test]
    async fn small_diagnostic_limit_preserves_all_frames_and_trailers_without_read_ahead() {
        let polls = Arc::new(AtomicUsize::new(0));
        let mut trailers = axum::http::HeaderMap::new();
        trailers.insert("x-trailer", "retained".parse().unwrap());
        let frames = Frames {
            polls: polls.clone(),
            frames: VecDeque::from([
                Ok(Frame::data(Bytes::from_static(b"0123456789"))),
                Ok(Frame::data(Bytes::from_static(b"ABCDEFGHIJ"))),
                Ok(Frame::trailers(trailers.clone())),
            ]),
        };
        let (mut body, handle) = capture_body(Body::new(frames), 8, None);
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        let first = futures::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.into_data().unwrap(), b"0123456789"[..]);
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        let second = futures::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.into_data().unwrap(), b"ABCDEFGHIJ"[..]);
        let trailer = futures::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(trailer.into_trailers().unwrap(), trailers);
        assert!(
            futures::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
                .await
                .is_none()
        );
        let captured = handle.take();
        assert_eq!(captured.bytes, b"01234567");
        assert_eq!(captured.observed_bytes, 20);
        assert!(captured.complete && captured.truncated && !captured.failed);
    }

    #[tokio::test]
    async fn original_body_error_reaches_consumer_and_finalization_runs_once() {
        let finishes = Arc::new(AtomicUsize::new(0));
        let finished = finishes.clone();
        let source = Frames {
            polls: Arc::default(),
            frames: VecDeque::from([
                Ok(Frame::data(Bytes::from_static(b"valid prefix"))),
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "test-only-original-error",
                )),
            ]),
        };
        let (body, _) = capture_body(
            Body::new(source),
            4,
            Some(Box::new(move |capture| {
                finished.fetch_add(1, Ordering::SeqCst);
                assert!(capture.failed && capture.truncated);
                assert_eq!(capture.bytes, b"vali");
            })),
        );
        let error = axum::body::to_bytes(body, 100).await.unwrap_err();
        assert!(format!("{error:?}").contains("test-only-original-error"));
        assert_eq!(finishes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn consumer_drop_does_not_poll_or_drain_the_source() {
        let finishes = Arc::new(AtomicUsize::new(0));
        let finished = finishes.clone();
        let polls = Arc::new(AtomicUsize::new(0));
        let source = Frames {
            polls: polls.clone(),
            frames: VecDeque::from([Ok(Frame::data(Bytes::from_static(b"never read")))]),
        };
        let (body, _) = capture_body(
            Body::new(source),
            4,
            Some(Box::new(move |capture| {
                finished.fetch_add(1, Ordering::SeqCst);
                assert!(!capture.complete && capture.bytes.is_empty());
            })),
        );
        drop(body);
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        assert_eq!(finishes.load(Ordering::SeqCst), 1);
    }

    struct ScopedFrames {
        context: Arc<crate::proxy::pipeline::budget::RequestContext>,
        sent: bool,
    }
    impl HttpBody for ScopedFrames {
        type Data = Bytes;
        type Error = std::io::Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            assert!(Arc::ptr_eq(
                &crate::proxy::pipeline::budget::current().unwrap(),
                &self.context
            ));
            if self.sent {
                return Poll::Ready(None);
            }
            self.sent = true;
            crate::proxy::monitor::CURRENT_UPSTREAM_CAPTURE
                .with(|capture| capture.record_dispatch());
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"scoped")))))
        }
    }
    impl Drop for ScopedFrames {
        fn drop(&mut self) {
            assert!(Arc::ptr_eq(
                &crate::proxy::pipeline::budget::current().unwrap(),
                &self.context
            ));
            crate::proxy::monitor::CURRENT_UPSTREAM_CAPTURE.with(|capture| {
                capture.record_usage(&serde_json::json!({"candidatesTokenCount":3}))
            });
        }
    }

    #[tokio::test]
    async fn lazy_body_poll_and_drop_restore_scopes_before_final_accounting() {
        let context = crate::proxy::pipeline::budget::RequestContext::new(
            crate::proxy::pipeline::budget::deadline_now(),
        );
        for consume in [true, false] {
            let holder = crate::proxy::monitor::UpstreamRequestBodyHolder::new();
            let finished_holder = holder.clone();
            let finishes = Arc::new(AtomicUsize::new(0));
            let finished = finishes.clone();
            let (body, _) = capture_body_in_scope(
                Body::new(ScopedFrames {
                    context: context.clone(),
                    sent: false,
                }),
                3,
                Some(Box::new(move |captured| {
                    finished.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(captured.complete, consume);
                    assert_eq!(finished_holder.physical_requests(), consume.then_some(1));
                    assert_eq!(finished_holder.usage().unwrap()["candidatesTokenCount"], 3);
                })),
                holder,
                Some(context.clone()),
            );
            if consume {
                assert_eq!(
                    axum::body::to_bytes(body, 100).await.unwrap(),
                    b"scoped"[..]
                );
            } else {
                drop(body);
            }
            assert_eq!(finishes.load(Ordering::SeqCst), 1);
            assert!(crate::proxy::pipeline::budget::current().is_none());
            assert!(crate::proxy::monitor::CURRENT_UPSTREAM_CAPTURE
                .try_with(|_| ())
                .is_err());
        }
    }
}
