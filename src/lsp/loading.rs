//! Negotiated, delayed loading progress with acknowledged token creation.
use super::*;

pub(super) struct LoadingProgress {
    enabled: bool,
    active: bool,
    started: std::time::Instant,
    next: u64,
    pending: Option<RequestId>,
    token: Option<String>,
    begun: bool,
    report_at: std::time::Instant,
}
impl LoadingProgress {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            active: false,
            started: std::time::Instant::now(),
            next: 0,
            pending: None,
            token: None,
            begun: false,
            report_at: std::time::Instant::now(),
        }
    }
    pub fn update(&mut self, active: bool, tx: &Sender<Message>) {
        if active && !self.active {
            self.started = std::time::Instant::now();
        }
        self.active = active;
        if active && self.begun && self.report_at.elapsed() >= std::time::Duration::from_millis(250)
        {
            self.notify(tx, serde_json::json!({"kind":"report", "message":"Discovering additional project inputs"}));
            self.report_at = std::time::Instant::now();
        }
        if !active && self.begun {
            self.notify(tx, serde_json::json!({"kind":"end", "message":"Project inputs ready; use tex-ls.inspectProject for details"}));
            self.begun = false;
            self.token = None;
        }
    }
    pub fn poll(&mut self, tx: &Sender<Message>) {
        if self.enabled
            && self.active
            && self.pending.is_none()
            && self.token.is_none()
            && self.started.elapsed() >= std::time::Duration::from_millis(150)
        {
            self.next += 1;
            let token = format!("tex-ls-loading-{}", self.next);
            let id = RequestId::from(token.clone());
            self.pending = Some(id.clone());
            self.token = Some(token.clone());
            let _ = tx.send(Message::Request(Request::new(
                id,
                "window/workDoneProgress/create".into(),
                serde_json::json!({"token":token}),
            )));
        }
    }
    pub fn response(&mut self, response: &Response, tx: &Sender<Message>) -> bool {
        if self.pending.as_ref() != Some(&response.id) {
            return false;
        }
        self.pending = None;
        if response.response_result.is_ok() && self.active {
            self.begun = true;
            self.notify(tx, serde_json::json!({"kind":"begin", "title":"Loading tex-ls project", "cancellable":false, "message":"Discovering source and TeX installation inputs"}));
        } else {
            self.token = None;
        }
        if response.response_result.is_err() {
            self.enabled = false;
        }
        true
    }
    fn notify(&self, tx: &Sender<Message>, value: serde_json::Value) {
        let _ = tx.send(Message::Notification(Notification::new(
            "$/progress".into(),
            serde_json::json!({"token":self.token,"value":value}),
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_is_delayed_acknowledged_and_quiet_for_short_work() {
        let (tx, rx) = unbounded();
        let mut loading = LoadingProgress::new(true);
        loading.update(true, &tx);
        loading.poll(&tx);
        assert!(rx.is_empty());
        loading.update(false, &tx);
        loading.poll(&tx);
        assert!(rx.is_empty());
        loading.update(true, &tx);
        loading.started -= std::time::Duration::from_secs(1);
        loading.poll(&tx);
        let Message::Request(create) = rx.recv().unwrap() else {
            panic!("create")
        };
        assert_eq!(create.method, "window/workDoneProgress/create");
        assert!(rx.is_empty());
        loading.response(&Response::new_ok(create.id, serde_json::Value::Null), &tx);
        let Message::Notification(begin) = rx.recv().unwrap() else {
            panic!("begin")
        };
        assert_eq!(begin.params["value"]["kind"], "begin");
        loading.report_at -= std::time::Duration::from_secs(1);
        loading.update(true, &tx);
        let Message::Notification(report) = rx.recv().unwrap() else {
            panic!("report")
        };
        assert_eq!(report.params["value"]["kind"], "report");
        loading.update(false, &tx);
        let Message::Notification(end) = rx.recv().unwrap() else {
            panic!("end")
        };
        assert_eq!(end.params["value"]["kind"], "end");
        loading.update(false, &tx);
        assert!(rx.is_empty());
        let mut unsupported = LoadingProgress::new(false);
        unsupported.update(true, &tx);
        unsupported.started -= std::time::Duration::from_secs(1);
        unsupported.poll(&tx);
        assert!(rx.is_empty());
    }
}
