use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BodyLengthKnowledge {
    Unchanged,
    Exact(usize),
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpFraming {
    Http1,
    Http2,
}

#[derive(Debug)]
pub struct FramingLedger {
    knowledge: BodyLengthKnowledge,
    framing_headers_mutated: bool,
    committed: bool,
}

impl Default for FramingLedger {
    fn default() -> Self {
        Self {
            knowledge: BodyLengthKnowledge::Unchanged,
            framing_headers_mutated: false,
            committed: false,
        }
    }
}

impl FramingLedger {
    pub fn knowledge(&self) -> BodyLengthKnowledge {
        self.knowledge
    }

    pub fn framing_headers_mutated(&self) -> bool {
        self.framing_headers_mutated
    }

    pub fn record_header_mutation(&mut self, name: &http::HeaderName) {
        if self.committed {
            return;
        }
        if name == CONTENT_LENGTH || name == TRANSFER_ENCODING {
            self.framing_headers_mutated = true;
            self.knowledge = BodyLengthKnowledge::Unknown;
        }
    }

    pub fn pass_through(&mut self) -> Result<(), BodyError> {
        self.ensure_held()
    }

    pub fn streaming_transform(&mut self) -> Result<(), BodyError> {
        self.ensure_held()?;
        self.knowledge = BodyLengthKnowledge::Unknown;
        Ok(())
    }

    pub fn buffered_eos(&mut self, exact: usize) -> Result<(), BodyError> {
        self.ensure_held()?;
        self.knowledge = BodyLengthKnowledge::Exact(exact);
        Ok(())
    }

    pub fn finalize(
        &mut self,
        headers: &mut HeaderMap,
        protocol: HttpFraming,
        method: Option<&Method>,
        status: Option<StatusCode>,
    ) -> Result<(), BodyError> {
        self.ensure_held()?;
        if self.framing_headers_mutated {
            // A filter-authored CL/TE value is never accepted as a length
            // fact. Rebuild framing solely from the emitter outcome below.
            headers.remove(CONTENT_LENGTH);
            headers.remove(TRANSFER_ENCODING);
        } else if headers.contains_key(CONTENT_LENGTH) && headers.contains_key(TRANSFER_ENCODING) {
            return Err(BodyError::ContentLengthTransferEncodingConflict);
        }
        let body_forbidden = method == Some(&Method::HEAD)
            || status.is_some_and(|status| {
                status.is_informational()
                    || status == StatusCode::NO_CONTENT
                    || status == StatusCode::NOT_MODIFIED
            });
        if body_forbidden {
            headers.remove(TRANSFER_ENCODING);
            headers.remove(CONTENT_LENGTH);
        } else {
            match self.knowledge {
                BodyLengthKnowledge::Unchanged => {
                    if protocol == HttpFraming::Http2 {
                        headers.remove(TRANSFER_ENCODING);
                    }
                }
                BodyLengthKnowledge::Exact(length) => {
                    headers.remove(TRANSFER_ENCODING);
                    headers.insert(
                        CONTENT_LENGTH,
                        HeaderValue::from_str(&length.to_string())
                            .map_err(|_| BodyError::InvalidContentLength)?,
                    );
                }
                BodyLengthKnowledge::Unknown => {
                    headers.remove(CONTENT_LENGTH);
                    match protocol {
                        HttpFraming::Http1 => {
                            headers.insert(TRANSFER_ENCODING, HeaderValue::from_static("chunked"));
                        }
                        HttpFraming::Http2 => {
                            headers.remove(TRANSFER_ENCODING);
                        }
                    }
                }
            }
        }
        self.committed = true;
        Ok(())
    }

    fn ensure_held(&self) -> Result<(), BodyError> {
        if self.committed {
            Err(BodyError::FramingAlreadyCommitted)
        } else {
            Ok(())
        }
    }
}

impl FramingLedgerPort for FramingLedger {
    fn header_mutated(&mut self, name: &http::HeaderName) {
        self.record_header_mutation(name);
    }

    fn body_transformed(&mut self) {
        if !self.committed {
            self.knowledge = BodyLengthKnowledge::Unknown;
        }
    }
}
