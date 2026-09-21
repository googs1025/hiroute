use super::framer::UTF8_BOM;
use super::*;

pub trait SseVisitor {
    fn on_event(
        &mut self,
        event: SseEventView<'_>,
        emitter: &mut BoundedEventEmitter<'_, '_>,
    ) -> Result<(), SseError>;
}

pub trait BoundedOutputSink {
    fn emit_borrowed(&mut self, bytes: &[u8]) -> Result<(), SseError>;
    fn emit_owned(&mut self, bytes: ChargedBytes) -> Result<(), SseError>;
}

pub struct BoundedEventEmitter<'a, 's> {
    pub(super) raw: &'a [u8],
    pub(super) limits: &'a SseLimits,
    pub(super) budget: &'a StreamBudget,
    pub(super) sink: &'s mut dyn BoundedOutputSink,
    pub(super) emitted: usize,
    pub(super) emitted_bytes: usize,
    pub(super) decided: bool,
}

impl BoundedEventEmitter<'_, '_> {
    pub fn pass_raw(&mut self) -> Result<(), SseError> {
        if self.decided {
            return Err(SseError::EmitterAlreadyDecided);
        }
        self.sink.emit_borrowed(self.raw)?;
        self.decided = true;
        Ok(())
    }

    pub fn drop_event(&mut self) -> Result<(), SseError> {
        if self.decided {
            return Err(SseError::EmitterAlreadyDecided);
        }
        self.decided = true;
        Ok(())
    }

    pub fn output_builder(&self, capacity: usize) -> Result<ChargedBytesBuilder, SseError> {
        let ratio_limit = self
            .raw
            .len()
            .saturating_mul(self.limits.expansion_ratio_numerator)
            / self.limits.expansion_ratio_denominator;
        let event_limit = ratio_limit.saturating_add(self.limits.expansion_slack_bytes);
        let next = self
            .emitted_visible_bytes()
            .checked_add(capacity)
            .ok_or(SseError::OutputLimit)?;
        if next > event_limit || next > self.limits.max_output_event_bytes {
            return Err(SseError::OutputLimit);
        }
        ChargedBytesBuilder::new(self.budget, MemoryRole::OutputQueue, capacity)
            .map_err(|_| SseError::BudgetExceeded)
    }

    pub fn emit_owned(&mut self, bytes: ChargedBytes) -> Result<(), SseError> {
        if self.decided && self.emitted == 0 {
            return Err(SseError::EmitterAlreadyDecided);
        }
        let ratio_limit = self
            .raw
            .len()
            .saturating_mul(self.limits.expansion_ratio_numerator)
            / self.limits.expansion_ratio_denominator;
        let event_limit = ratio_limit.saturating_add(self.limits.expansion_slack_bytes);
        let next = self
            .emitted_visible_bytes()
            .checked_add(bytes.bytes().len())
            .ok_or(SseError::OutputLimit)?;
        if next > event_limit || next > self.limits.max_output_event_bytes {
            return Err(SseError::OutputLimit);
        }
        if bytes.role() != MemoryRole::OutputQueue {
            return Err(SseError::BudgetExceeded);
        }
        self.sink.emit_owned(bytes)?;
        self.emitted += 1;
        self.emitted_bytes = next;
        self.decided = true;
        Ok(())
    }

    fn emitted_visible_bytes(&self) -> usize {
        self.emitted_bytes
    }
}

#[derive(Debug)]
pub struct SseEventView<'a> {
    pub(super) raw: &'a [u8],
    pub(super) strip_bom: bool,
    pub(super) _not_send: PhantomData<Rc<()>>,
}

#[derive(Debug)]
pub enum SseData<'a> {
    Borrowed(&'a [u8]),
    Owned(ChargedBytes),
}

impl AsRef<[u8]> for SseData<'_> {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Owned(bytes) => bytes.bytes(),
        }
    }
}

impl<'a> SseEventView<'a> {
    pub fn raw(&self) -> &'a [u8] {
        self.raw
    }

    pub fn fields(&self) -> SseFieldIter<'a> {
        let bytes = if self.strip_bom {
            &self.raw[UTF8_BOM.len()..]
        } else {
            self.raw
        };
        SseFieldIter { bytes, cursor: 0 }
    }

    pub fn event_type(&self) -> Option<&'a [u8]> {
        let mut value = None;
        for field in self.fields() {
            if field.name == b"event" {
                value = Some(field.value);
            }
        }
        value
    }

    pub fn id(&self) -> Option<&'a [u8]> {
        let mut value = None;
        for field in self.fields() {
            if field.name == b"id" && !field.value.contains(&0) {
                value = Some(field.value);
            }
        }
        value
    }

    pub fn retry(&self) -> Option<u64> {
        let mut value = None;
        for field in self.fields() {
            if field.name == b"retry"
                && let Some(parsed) = parse_ascii_u64(field.value)
            {
                value = Some(parsed);
            }
        }
        value
    }

    pub fn data(&self, budget: &StreamBudget) -> Result<SseData<'a>, SseError> {
        let mut values = self
            .fields()
            .filter(|field| field.name == b"data")
            .map(|field| field.value);
        let Some(first) = values.next() else {
            return Ok(SseData::Borrowed(&[]));
        };
        let Some(second) = values.next() else {
            return Ok(SseData::Borrowed(first));
        };
        let capacity = first
            .len()
            .checked_add(second.len().saturating_add(1))
            .and_then(|size| {
                values
                    .clone()
                    .try_fold(size, |size, value| size.checked_add(value.len() + 1))
            })
            .ok_or(SseError::BudgetExceeded)?;
        let mut joined = ChargedBytesBuilder::new(budget, MemoryRole::SemanticState, capacity)
            .map_err(|_| SseError::BudgetExceeded)?;
        joined
            .extend_from_slice(first)
            .map_err(|_| SseError::BudgetExceeded)?;
        for value in std::iter::once(second).chain(values) {
            joined.push(b'\n').map_err(|_| SseError::BudgetExceeded)?;
            joined
                .extend_from_slice(value)
                .map_err(|_| SseError::BudgetExceeded)?;
        }
        Ok(SseData::Owned(joined.finish()))
    }

    pub fn promote(&self, budget: &StreamBudget) -> Result<OwnedSseEvent, SseError> {
        Ok(OwnedSseEvent {
            raw: ChargedBytes::copy_from_opaque(budget, MemoryRole::SseFrame, self.raw)
                .map_err(|_| SseError::BudgetExceeded)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SseField<'a> {
    pub name: &'a [u8],
    pub value: &'a [u8],
    pub comment: bool,
}

#[derive(Clone, Debug)]
pub struct SseFieldIter<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Iterator for SseFieldIter<'a> {
    type Item = SseField<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.cursor < self.bytes.len() {
            let start = self.cursor;
            while self.cursor < self.bytes.len()
                && self.bytes[self.cursor] != b'\r'
                && self.bytes[self.cursor] != b'\n'
            {
                self.cursor += 1;
            }
            let line = &self.bytes[start..self.cursor];
            if self.cursor < self.bytes.len() {
                if self.bytes[self.cursor] == b'\r' {
                    self.cursor += 1;
                    if self.cursor < self.bytes.len() && self.bytes[self.cursor] == b'\n' {
                        self.cursor += 1;
                    }
                } else {
                    self.cursor += 1;
                }
            }
            if line.is_empty() {
                continue;
            }
            if let Some(comment) = line.strip_prefix(b":") {
                return Some(SseField {
                    name: b"",
                    value: comment.strip_prefix(b" ").unwrap_or(comment),
                    comment: true,
                });
            }
            let (name, value) = match line.iter().position(|byte| *byte == b':') {
                Some(colon) => {
                    let value = &line[colon + 1..];
                    (&line[..colon], value.strip_prefix(b" ").unwrap_or(value))
                }
                None => (line, &[][..]),
            };
            return Some(SseField {
                name,
                value,
                comment: false,
            });
        }
        None
    }
}

fn parse_ascii_u64(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    bytes.iter().try_fold(0_u64, |value, byte| {
        value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))
    })
}
