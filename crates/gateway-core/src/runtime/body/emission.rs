use super::*;

#[derive(Debug)]
pub struct BodyEmitter {
    ledger: FramingLedger,
    output: Vec<ChargedBytes>,
}

impl Default for BodyEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl BodyEmitter {
    pub fn new() -> Self {
        Self {
            ledger: FramingLedger::default(),
            output: Vec::new(),
        }
    }

    pub fn pass(&mut self, bytes: ChargedBytes) -> Result<(), BodyError> {
        self.ledger.pass_through()?;
        self.output.push(bytes);
        Ok(())
    }

    pub fn drop_unit(&mut self, _bytes: ChargedBytes) -> Result<(), BodyError> {
        self.ledger.streaming_transform()
    }

    pub fn emit_transformed(&mut self, bytes: ChargedBytes) -> Result<(), BodyError> {
        self.ledger.streaming_transform()?;
        self.output.push(bytes);
        Ok(())
    }

    pub fn complete_buffered(&mut self) -> Result<(), BodyError> {
        let exact = self.output.iter().map(|chunk| chunk.bytes().len()).sum();
        self.ledger.buffered_eos(exact)
    }

    pub fn ledger(&self) -> &FramingLedger {
        &self.ledger
    }

    pub fn ledger_mut(&mut self) -> &mut FramingLedger {
        &mut self.ledger
    }

    pub fn output(&self) -> &[ChargedBytes] {
        &self.output
    }
}
