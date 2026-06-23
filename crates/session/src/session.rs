use gpui::Context;

pub struct Session {
    session_id: String,
}

impl Session {
    pub async fn new(session_id: String) -> Self {
        Self {
            session_id,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test() -> Self {
        Self {
            session_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    pub fn id(&self) -> &str {
        &self.session_id
    }
}

pub struct AppSession {
    session: Session,
}

impl AppSession {
    pub fn new(session: Session, _cx: &Context<Self>) -> Self {
        Self {
            session,
        }
    }

    pub fn id(&self) -> &str {
        self.session.id()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_session_for_test(&mut self, session: Session) {
        self.session = session;
    }
}
