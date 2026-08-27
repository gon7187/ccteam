use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::{collections::VecDeque, sync::Mutex};

use async_trait::async_trait;
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, EventAttachment, ExecutionMode,
    HarnessAdapter, HarnessError, PermissionMode, SessionProtocol, SpawnCtx, ThreadEvent,
    ThreadHandle, ThreadStatus, ToolSurfaceRebuild, TurnInput, TurnRouting, TurnSubmission,
};
use ccteam_im::gateway::{Gateway, GatewayDeadline, GatewayRequestError, SpawnTuning};
use futures::stream::BoxStream;

#[derive(Debug, Default)]
struct DeadlineAdapter {
    submissions: AtomicUsize,
    interrupts: AtomicUsize,
    stall_submit: AtomicBool,
    emit_started_before_stall: AtomicBool,
    events: Arc<Mutex<VecDeque<ThreadEvent>>>,
    events_notify: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl HarnessAdapter for DeadlineAdapter {
    fn name(&self) -> &'static str {
        "deadline-test"
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Claude
    }

    async fn start_thread(
        &self,
        _spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: format!("deadline-{}", ctx.sid),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::json!({}),
        })
    }

    async fn submit_turn_routed(
        &self,
        _handle: &ThreadHandle,
        _input: TurnInput,
        _routing: TurnRouting,
    ) -> Result<TurnSubmission, HarnessError> {
        let sequence = self.submissions.fetch_add(1, Ordering::SeqCst) + 1;
        if self.emit_started_before_stall.load(Ordering::SeqCst) {
            self.events
                .lock()
                .unwrap()
                .push_back(ThreadEvent::TurnStarted {
                    turn_id: format!("turn-{sequence}"),
                });
            self.events_notify.notify_waiters();
        }
        if self.stall_submit.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(TurnSubmission::started(ccteam_harness::TurnId::new(
            format!("turn-{sequence}"),
        )))
    }

    fn events(&self, _handle: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        let events = Arc::clone(&self.events);
        let notify = Arc::clone(&self.events_notify);
        Box::pin(futures::stream::unfold((), move |_| {
            let events = Arc::clone(&events);
            let notify = Arc::clone(&notify);
            async move {
                loop {
                    if let Some(event) = events.lock().unwrap().pop_front() {
                        return Some((event, ()));
                    }
                    notify.notified().await;
                }
            }
        }))
    }

    fn event_attachment(&self) -> EventAttachment {
        EventAttachment::OneShot
    }

    async fn rebuild_tool_surface(
        &self,
        _handle: &ThreadHandle,
    ) -> Result<ToolSurfaceRebuild, HarnessError> {
        Ok(ToolSurfaceRebuild::RespawnRequired {
            reason: "test adapter".to_string(),
        })
    }

    async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "test adapter".to_string(),
        })
    }

    async fn close_thread(&self, _handle: &ThreadHandle) -> Result<(), HarnessError> {
        Ok(())
    }

    async fn interrupt_turn(&self, _handle: &ThreadHandle) -> Result<(), HarnessError> {
        self.interrupts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn handle_directive(
        &self,
        _handle: &ThreadHandle,
        _directive: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        Ok(DirectiveOutcome::Rejected {
            reason: "test adapter".to_string(),
        })
    }

    async fn thread_status(&self, _handle: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        Ok(ThreadStatus::default())
    }
}

fn assert_code(error: &anyhow::Error, expected: &str) {
    let classified = error
        .downcast_ref::<GatewayRequestError>()
        .expect("gateway request error remains downcastable");
    assert_eq!(classified.error_code(), expected);
}

#[tokio::test]
async fn queue_and_vendor_deadlines_are_split_and_leave_gateway_healthy() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let ccteam_home = tmp.path().join("ccteam-home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).expect("create isolated HOME");
    std::fs::create_dir_all(&ccteam_home).expect("create isolated CCTEAM_HOME");
    std::fs::create_dir_all(&project).expect("create project");
    std::env::set_var("HOME", &home);
    std::env::set_var("CCTEAM_HOME", &ccteam_home);
    std::env::set_var("CCTEAM_GATEWAY_QUEUE_DEADLINE_MS", "40");
    std::env::set_var("CCTEAM_IM_GATEWAY_SUBMIT_TIMEOUT_MS", "40");

    let adapter = Arc::new(DeadlineAdapter::default());
    let gateway = Arc::new(tokio::sync::Mutex::new(Gateway::new(
        Arc::clone(&adapter) as Arc<dyn HarnessAdapter + Send + Sync>,
        "alpha",
        &project,
    )));
    let created = Gateway::create_session_api_tuned_shared(
        Arc::clone(&gateway),
        "alpha".to_string(),
        String::new(),
        AgentVendor::Claude,
        PermissionMode::Skip,
        SessionProtocol::StreamJson,
        "web-api".to_string(),
        SpawnTuning::default(),
        GatewayDeadline::start(),
    )
    .await
    .expect("seed live session");

    let held = gateway.lock().await;
    let queued_gateway = Arc::clone(&gateway);
    let queued_sid = created.sid.clone();
    let queued = tokio::spawn(async move {
        Gateway::submit_to_sid_shared(
            queued_gateway,
            &queued_sid,
            "queued".to_string(),
            GatewayDeadline::start(),
        )
        .await
    });
    let queued_error = tokio::time::timeout(Duration::from_secs(1), queued)
        .await
        .expect("queue deadline fires while lock remains held")
        .expect("submit task joins")
        .expect_err("lock wait must fail before vendor entry");
    assert_code(&queued_error, "gateway_queue_deadline");
    assert_eq!(adapter.submissions.load(Ordering::SeqCst), 0);
    drop(held);

    std::env::set_var("CCTEAM_GATEWAY_QUEUE_DEADLINE_MS", "1000");
    tokio::time::timeout(Duration::from_secs(1), async {
        assert_eq!(gateway.lock().await.session_views().len(), 1);
        Gateway::submit_to_sid_shared(
            Arc::clone(&gateway),
            &created.sid,
            "healthy-after-queue".to_string(),
            GatewayDeadline::start(),
        )
        .await
    })
    .await
    .expect("status and normal turn remain prompt")
    .expect("normal turn after queue failure");

    adapter.stall_submit.store(true, Ordering::SeqCst);
    let vendor_error = Gateway::submit_to_sid_shared(
        Arc::clone(&gateway),
        &created.sid,
        "slow-vendor".to_string(),
        GatewayDeadline::start(),
    )
    .await
    .expect_err("vendor timeout must be classified independently");
    assert_code(&vendor_error, "vendor_submit_timeout");

    assert!(
        gateway.lock().await.session_turn_in_flight(&created.sid),
        "a dispatched request with a missing acknowledgement remains in flight"
    );
    let rendered = vendor_error.to_string();
    assert!(rendered.contains("outcome is unknown"), "{rendered}");
    assert!(rendered.contains("do not retry"), "{rendered}");
    assert!(rendered.contains("/status"), "{rendered}");

    adapter.stall_submit.store(false, Ordering::SeqCst);
    let retry_error = Gateway::submit_to_sid_shared(
        Arc::clone(&gateway),
        &created.sid,
        "blind-retry".to_string(),
        GatewayDeadline::start(),
    )
    .await
    .expect_err("an outcome-unknown turn must reject a blind second dispatch");
    assert_code(&retry_error, "vendor_submit_timeout");
    assert_eq!(retry_error.to_string(), rendered);

    assert_eq!(adapter.submissions.load(Ordering::SeqCst), 2);

    let interrupted = Gateway::submit_to_sid_shared(
        Arc::clone(&gateway),
        &created.sid,
        "/interrupt".to_string(),
        GatewayDeadline::start(),
    )
    .await
    .expect("emergency interrupt bypasses the blind user-payload guard");
    assert_eq!(interrupted, format!("directive:{}", created.sid));
    assert_eq!(adapter.interrupts.load(Ordering::SeqCst), 1);
    assert_eq!(adapter.submissions.load(Ordering::SeqCst), 2);

    let status = tokio::time::timeout(
        Duration::from_secs(1),
        Gateway::handle_observability_shared(
            Arc::clone(&gateway),
            "web",
            "web-api",
            "web-api",
            "/status",
        ),
    )
    .await
    .expect("outcome-unknown state never blocks reconciliation status")
    .expect("status remains available");
    assert_eq!(status.len(), 1);

    let stopped = Gateway::handle_message_shared(
        Arc::clone(&gateway),
        "web",
        "web-api",
        "web-api",
        "stop-after-unknown",
        &format!("/stop {}", created.sid),
        &[],
        None,
    )
    .await
    .expect("outcome-unknown state never blocks explicit stop");
    assert!(stopped[0].plain.contains("остановлена"), "{stopped:?}");

    let reconciled_adapter = Arc::new(DeadlineAdapter::default());
    let reconciled_gateway = Arc::new(tokio::sync::Mutex::new(Gateway::new(
        Arc::clone(&reconciled_adapter) as Arc<dyn HarnessAdapter + Send + Sync>,
        "alpha",
        &project,
    )));
    let (events, _events_rx) = tokio::sync::mpsc::unbounded_channel();
    reconciled_gateway.lock().await.set_event_sink(events);
    let reconciled = Gateway::create_session_api_tuned_shared(
        Arc::clone(&reconciled_gateway),
        "alpha".to_string(),
        String::new(),
        AgentVendor::Claude,
        PermissionMode::Skip,
        SessionProtocol::StreamJson,
        "web-api".to_string(),
        SpawnTuning::default(),
        GatewayDeadline::start(),
    )
    .await
    .expect("seed reconciled session");
    reconciled_adapter
        .emit_started_before_stall
        .store(true, Ordering::SeqCst);
    reconciled_adapter
        .stall_submit
        .store(true, Ordering::SeqCst);
    Gateway::submit_to_sid_shared(
        Arc::clone(&reconciled_gateway),
        &reconciled.sid,
        "started-before-ack".to_string(),
        GatewayDeadline::start(),
    )
    .await
    .expect_err("ack still times out after canonical start");
    reconciled_adapter
        .stall_submit
        .store(false, Ordering::SeqCst);
    Gateway::submit_to_sid_shared(
        Arc::clone(&reconciled_gateway),
        &reconciled.sid,
        "follow-up-after-reconciliation".to_string(),
        GatewayDeadline::start(),
    )
    .await
    .expect("canonical TurnStarted wins the timeout race");
    assert_eq!(reconciled_adapter.submissions.load(Ordering::SeqCst), 2);
}
