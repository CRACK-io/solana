#![cfg(feature = "full")]

use itertools::Itertools;
use solana_sdk::{
    account::AccountSharedData,
    compute_budget::ComputeBudget,
    feature_set::remove_native_loader,
    fee_calculator::FeeCalculator,
    hash::Hash,
    instruction::{CompiledInstruction, Instruction, InstructionError},
    keyed_account::{create_keyed_accounts_unified, KeyedAccount},
    message::Message,
    pubkey::Pubkey,
    sysvar::Sysvar,
};
use std::{cell::RefCell, collections::HashSet, fmt::Debug, rc::Rc, sync::Arc};

/// Prototype of a native loader entry point
///
/// program_id: Program ID of the currently executing program
/// keyed_accounts: Accounts passed as part of the instruction
/// instruction_data: Instruction data
/// invoke_context: Invocation context
pub type LoaderEntrypoint = unsafe extern "C" fn(
    program_id: &Pubkey,
    instruction_data: &[u8],
    invoke_context: &dyn InvokeContext,
) -> Result<(), InstructionError>;

pub type ProcessInstructionWithContext =
    fn(usize, &[u8], &mut dyn InvokeContext) -> Result<(), InstructionError>;

pub struct InvokeContextStackFrame<'a> {
    pub key: Pubkey,
    pub keyed_accounts: Vec<KeyedAccount<'a>>,
    pub keyed_accounts_range: std::ops::Range<usize>,
}

impl<'a> InvokeContextStackFrame<'a> {
    pub fn new(key: Pubkey, keyed_accounts: Vec<KeyedAccount<'a>>) -> Self {
        let keyed_accounts_range = std::ops::Range {
            start: 0,
            end: keyed_accounts.len(),
        };
        Self {
            key,
            keyed_accounts,
            keyed_accounts_range,
        }
    }
}

/// Invocation context passed to loaders
pub trait InvokeContext {
    /// Push a stack frame onto the invocation stack
    fn push(
        &mut self,
        key: &Pubkey,
        message: &Message,
        instruction: &CompiledInstruction,
        program_indices: &[usize],
        account_indices: Option<&[usize]>,
    ) -> Result<(), InstructionError>;
    /// Pop a stack frame from the invocation stack
    fn pop(&mut self);
    /// Current depth of the invocation stake
    fn invoke_depth(&self) -> usize;
    /// Verify the results of an instruction
    fn verify(
        &mut self,
        message: &Message,
        instruction: &CompiledInstruction,
        program_indices: &[usize],
    ) -> Result<(), InstructionError>;
    /// Verify and update PreAccount state based on program execution
    fn verify_and_update(
        &mut self,
        instruction: &CompiledInstruction,
        account_indices: &[usize],
        write_privileges: &[bool],
    ) -> Result<(), InstructionError>;
    /// Get the program ID of the currently executing program
    fn get_caller(&self) -> Result<&Pubkey, InstructionError>;
    /// Removes the first keyed account
    #[deprecated(
        since = "1.9.0",
        note = "To be removed together with remove_native_loader"
    )]
    fn remove_first_keyed_account(&mut self) -> Result<(), InstructionError>;
    /// Get the list of keyed accounts
    fn get_keyed_accounts(&self) -> Result<&[KeyedAccount], InstructionError>;
    /// Get a list of built-in programs
    fn get_programs(&self) -> &[(Pubkey, ProcessInstructionWithContext)];
    /// Get this invocation's logger
    fn get_logger(&self) -> Rc<RefCell<dyn Logger>>;
    /// Get this invocation's compute budget
    #[allow(deprecated)]
    #[deprecated(since = "1.8.0", note = "please use `get_compute_budget` instead")]
    fn get_bpf_compute_budget(&self) -> &BpfComputeBudget;
    /// Get this invocation's compute meter
    fn get_compute_meter(&self) -> Rc<RefCell<dyn ComputeMeter>>;
    /// Loaders may need to do work in order to execute a program.  Cache
    /// the work that can be re-used across executions
    fn add_executor(&self, pubkey: &Pubkey, executor: Arc<dyn Executor>);
    /// Get the completed loader work that can be re-used across executions
    fn get_executor(&self, pubkey: &Pubkey) -> Option<Arc<dyn Executor>>;
    /// Set which instruction in the message is currently being recorded
    fn set_instruction_index(&mut self, instruction_index: usize);
    /// Record invoked instruction
    fn record_instruction(&self, instruction: &Instruction);
    /// Get the bank's active feature set
    fn is_feature_active(&self, feature_id: &Pubkey) -> bool;
    /// Find an account_index and account by its key
    fn get_account(&self, pubkey: &Pubkey) -> Option<(usize, Rc<RefCell<AccountSharedData>>)>;
    /// Update timing
    fn update_timing(
        &mut self,
        serialize_us: u64,
        create_vm_us: u64,
        execute_us: u64,
        deserialize_us: u64,
    );
    /// Get sysvar data
    fn get_sysvar_data(&self, id: &Pubkey) -> Option<Rc<Vec<u8>>>;
    /// Get this invocation's compute budget
    fn get_compute_budget(&self) -> &ComputeBudget;
    /// Get this invocation's blockhash
    fn get_blockhash(&self) -> &Hash;
    /// Get this invocation's `FeeCalculator`
    fn get_fee_calculator(&self) -> &FeeCalculator;
    /// Set the return data
    fn set_return_data(&mut self, data: Vec<u8>) -> Result<(), InstructionError>;
    /// Get the return data
    fn get_return_data(&self) -> (Pubkey, &[u8]);
}

/// Convenience macro to log a message with an `Rc<RefCell<dyn Logger>>`
#[macro_export]
macro_rules! ic_logger_msg {
    ($logger:expr, $message:expr) => {
        if let Ok(logger) = $logger.try_borrow_mut() {
            if logger.log_enabled() {
                logger.log($message);
            }
        }
    };
    ($logger:expr, $fmt:expr, $($arg:tt)*) => {
        if let Ok(logger) = $logger.try_borrow_mut() {
            if logger.log_enabled() {
                logger.log(&format!($fmt, $($arg)*));
            }
        }
    };
}

/// Convenience macro to log a message with an `InvokeContext`
#[macro_export]
macro_rules! ic_msg {
    ($invoke_context:expr, $message:expr) => {
        $crate::ic_logger_msg!($invoke_context.get_logger(), $message)
    };
    ($invoke_context:expr, $fmt:expr, $($arg:tt)*) => {
        $crate::ic_logger_msg!($invoke_context.get_logger(), $fmt, $($arg)*)
    };
}

pub fn get_sysvar<T: Sysvar>(
    invoke_context: &dyn InvokeContext,
    id: &Pubkey,
) -> Result<T, InstructionError> {
    let sysvar_data = invoke_context.get_sysvar_data(id).ok_or_else(|| {
        ic_msg!(invoke_context, "Unable to get sysvar {}", id);
        InstructionError::UnsupportedSysvar
    })?;

    bincode::deserialize(&sysvar_data).map_err(|err| {
        ic_msg!(invoke_context, "Unable to get sysvar {}: {:?}", id, err);
        InstructionError::UnsupportedSysvar
    })
}

#[deprecated(since = "1.8.0", note = "please use `ComputeBudget` instead")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BpfComputeBudget {
    /// Number of compute units that an instruction is allowed.  Compute units
    /// are consumed by program execution, resources they use, etc...
    pub max_units: u64,
    /// Number of compute units consumed by a log_u64 call
    pub log_64_units: u64,
    /// Number of compute units consumed by a create_program_address call
    pub create_program_address_units: u64,
    /// Number of compute units consumed by an invoke call (not including the cost incurred by
    /// the called program)
    pub invoke_units: u64,
    /// Maximum cross-program invocation depth allowed
    pub max_invoke_depth: usize,
    /// Base number of compute units consumed to call SHA256
    pub sha256_base_cost: u64,
    /// Incremental number of units consumed by SHA256 (based on bytes)
    pub sha256_byte_cost: u64,
    /// Maximum BPF to BPF call depth
    pub max_call_depth: usize,
    /// Size of a stack frame in bytes, must match the size specified in the LLVM BPF backend
    pub stack_frame_size: usize,
    /// Number of compute units consumed by logging a `Pubkey`
    pub log_pubkey_units: u64,
    /// Maximum cross-program invocation instruction size
    pub max_cpi_instruction_size: usize,
    /// Number of account data bytes per conpute unit charged during a cross-program invocation
    pub cpi_bytes_per_unit: u64,
    /// Base number of compute units consumed to get a sysvar
    pub sysvar_base_cost: u64,
    /// Number of compute units consumed to call secp256k1_recover
    pub secp256k1_recover_cost: u64,
    /// Number of compute units consumed to do a syscall without any work
    pub syscall_base_cost: u64,
    /// Optional program heap region size, if `None` then loader default
    pub heap_size: Option<usize>,
}
#[allow(deprecated)]
impl From<ComputeBudget> for BpfComputeBudget {
    fn from(item: ComputeBudget) -> Self {
        BpfComputeBudget {
            max_units: item.max_units,
            log_64_units: item.log_64_units,
            create_program_address_units: item.create_program_address_units,
            invoke_units: item.invoke_units,
            max_invoke_depth: item.max_invoke_depth,
            sha256_base_cost: item.sha256_base_cost,
            sha256_byte_cost: item.sha256_byte_cost,
            max_call_depth: item.max_call_depth,
            stack_frame_size: item.stack_frame_size,
            log_pubkey_units: item.log_pubkey_units,
            max_cpi_instruction_size: item.max_cpi_instruction_size,
            cpi_bytes_per_unit: item.cpi_bytes_per_unit,
            sysvar_base_cost: item.sysvar_base_cost,
            syscall_base_cost: item.syscall_base_cost,
            secp256k1_recover_cost: item.secp256k1_recover_cost,
            heap_size: item.heap_size,
        }
    }
}
#[allow(deprecated)]
impl From<BpfComputeBudget> for ComputeBudget {
    fn from(item: BpfComputeBudget) -> Self {
        ComputeBudget {
            max_units: item.max_units,
            log_64_units: item.log_64_units,
            create_program_address_units: item.create_program_address_units,
            invoke_units: item.invoke_units,
            max_invoke_depth: item.max_invoke_depth,
            sha256_base_cost: item.sha256_base_cost,
            sha256_byte_cost: item.sha256_byte_cost,
            max_call_depth: item.max_call_depth,
            stack_frame_size: item.stack_frame_size,
            log_pubkey_units: item.log_pubkey_units,
            max_cpi_instruction_size: item.max_cpi_instruction_size,
            cpi_bytes_per_unit: item.cpi_bytes_per_unit,
            sysvar_base_cost: item.sysvar_base_cost,
            secp256k1_recover_cost: item.secp256k1_recover_cost,
            syscall_base_cost: item.syscall_base_cost,
            heap_size: item.heap_size,
        }
    }
}
#[allow(deprecated)]
impl Default for BpfComputeBudget {
    fn default() -> Self {
        ComputeBudget::default().into()
    }
}
#[allow(deprecated)]
impl BpfComputeBudget {
    pub fn new() -> Self {
        BpfComputeBudget::default()
    }
}

/// Compute meter
pub trait ComputeMeter {
    /// Consume compute units
    fn consume(&mut self, amount: u64) -> Result<(), InstructionError>;
    /// Get the number of remaining compute units
    fn get_remaining(&self) -> u64;
}

/// Log messages
pub trait Logger {
    fn log_enabled(&self) -> bool;

    /// Log a message.
    ///
    /// Unless explicitly stated, log messages are not considered stable and may change in the
    /// future as necessary
    fn log(&self, message: &str);
}

///
/// Stable program log messages
///
/// The format of these log messages should not be modified to avoid breaking downstream consumers
/// of program logging
///
pub mod stable_log {
    use super::*;

    /// Log a program invoke.
    ///
    /// The general form is:
    ///
    /// ```notrust
    /// "Program <address> invoke [<depth>]"
    /// ```
    pub fn program_invoke(
        logger: &Rc<RefCell<dyn Logger>>,
        program_id: &Pubkey,
        invoke_depth: usize,
    ) {
        ic_logger_msg!(logger, "Program {} invoke [{}]", program_id, invoke_depth);
    }

    /// Log a message from the program itself.
    ///
    /// The general form is:
    ///
    /// ```notrust
    /// "Program log: <program-generated output>"
    /// ```
    ///
    /// That is, any program-generated output is guaranteed to be prefixed by "Program log: "
    pub fn program_log(logger: &Rc<RefCell<dyn Logger>>, message: &str) {
        ic_logger_msg!(logger, "Program log: {}", message);
    }

    /// Emit a program data.
    ///
    /// The general form is:
    ///
    /// ```notrust
    /// "Program data: <binary-data-in-base64>*"
    /// ```
    ///
    /// That is, any program-generated output is guaranteed to be prefixed by "Program data: "
    pub fn program_data(logger: &Rc<RefCell<dyn Logger>>, data: &[&[u8]]) {
        ic_logger_msg!(
            logger,
            "Program data: {}",
            data.iter().map(base64::encode).join(" ")
        );
    }

    /// Log return data as from the program itself. This line will not be present if no return
    /// data was set, or if the return data was set to zero length.
    ///
    /// The general form is:
    ///
    /// ```notrust
    /// "Program return: <program-id> <program-generated-data-in-base64>"
    /// ```
    ///
    /// That is, any program-generated output is guaranteed to be prefixed by "Program return: "
    pub fn program_return(logger: &Rc<RefCell<dyn Logger>>, program_id: &Pubkey, data: &[u8]) {
        ic_logger_msg!(
            logger,
            "Program return: {} {}",
            program_id,
            base64::encode(data)
        );
    }

    /// Log successful program execution.
    ///
    /// The general form is:
    ///
    /// ```notrust
    /// "Program <address> success"
    /// ```
    pub fn program_success(logger: &Rc<RefCell<dyn Logger>>, program_id: &Pubkey) {
        ic_logger_msg!(logger, "Program {} success", program_id);
    }

    /// Log program execution failure
    ///
    /// The general form is:
    ///
    /// ```notrust
    /// "Program <address> failed: <program error details>"
    /// ```
    pub fn program_failure(
        logger: &Rc<RefCell<dyn Logger>>,
        program_id: &Pubkey,
        err: &InstructionError,
    ) {
        ic_logger_msg!(logger, "Program {} failed: {}", program_id, err);
    }
}

/// Program executor
pub trait Executor: Debug + Send + Sync {
    /// Execute the program
    fn execute(
        &self,
        first_instruction_account: usize,
        instruction_data: &[u8],
        invoke_context: &mut dyn InvokeContext,
        use_jit: bool,
    ) -> Result<(), InstructionError>;
}

#[derive(Debug, Default, Clone)]
pub struct MockComputeMeter {
    pub remaining: u64,
}
impl ComputeMeter for MockComputeMeter {
    fn consume(&mut self, amount: u64) -> Result<(), InstructionError> {
        let exceeded = self.remaining < amount;
        self.remaining = self.remaining.saturating_sub(amount);
        if exceeded {
            return Err(InstructionError::ComputationalBudgetExceeded);
        }
        Ok(())
    }
    fn get_remaining(&self) -> u64 {
        self.remaining
    }
}

#[derive(Debug, Default, Clone)]
pub struct MockLogger {
    pub log: Rc<RefCell<Vec<String>>>,
}
impl Logger for MockLogger {
    fn log_enabled(&self) -> bool {
        true
    }
    fn log(&self, message: &str) {
        self.log.borrow_mut().push(message.to_string());
    }
}

#[allow(deprecated)]
pub struct MockInvokeContext<'a> {
    pub invoke_stack: Vec<InvokeContextStackFrame<'a>>,
    pub logger: MockLogger,
    pub compute_budget: ComputeBudget,
    pub bpf_compute_budget: BpfComputeBudget,
    pub compute_meter: MockComputeMeter,
    pub programs: Vec<(Pubkey, ProcessInstructionWithContext)>,
    pub accounts: Vec<(Pubkey, Rc<RefCell<AccountSharedData>>)>,
    pub sysvars: Vec<(Pubkey, Option<Rc<Vec<u8>>>)>,
    pub disabled_features: HashSet<Pubkey>,
    pub blockhash: Hash,
    pub fee_calculator: FeeCalculator,
    pub return_data: (Pubkey, Vec<u8>),
}

impl<'a> MockInvokeContext<'a> {
    pub fn new(program_id: &Pubkey, keyed_accounts: Vec<KeyedAccount<'a>>) -> Self {
        let compute_budget = ComputeBudget::default();
        let mut invoke_context = MockInvokeContext {
            invoke_stack: Vec::with_capacity(compute_budget.max_invoke_depth),
            logger: MockLogger::default(),
            compute_budget,
            bpf_compute_budget: compute_budget.into(),
            compute_meter: MockComputeMeter {
                remaining: std::i64::MAX as u64,
            },
            programs: vec![],
            accounts: vec![],
            sysvars: vec![],
            disabled_features: HashSet::default(),
            blockhash: Hash::default(),
            fee_calculator: FeeCalculator::default(),
            return_data: (Pubkey::default(), Vec::new()),
        };
        invoke_context
            .invoke_stack
            .push(InvokeContextStackFrame::new(*program_id, keyed_accounts));
        invoke_context
    }
}

pub fn mock_set_sysvar<T: Sysvar>(
    mock_invoke_context: &mut MockInvokeContext,
    id: Pubkey,
    sysvar: T,
) -> Result<(), InstructionError> {
    let mut data = Vec::with_capacity(T::size_of());

    bincode::serialize_into(&mut data, &sysvar).map_err(|err| {
        ic_msg!(mock_invoke_context, "Unable to serialize sysvar: {:?}", err);
        InstructionError::GenericError
    })?;
    mock_invoke_context.sysvars.push((id, Some(Rc::new(data))));
    Ok(())
}

impl<'a> InvokeContext for MockInvokeContext<'a> {
    fn push(
        &mut self,
        _key: &Pubkey,
        _message: &Message,
        _instruction: &CompiledInstruction,
        _program_indices: &[usize],
        _account_indices: Option<&[usize]>,
    ) -> Result<(), InstructionError> {
        self.invoke_stack.push(InvokeContextStackFrame::new(
            *_key,
            create_keyed_accounts_unified(&[]),
        ));
        Ok(())
    }
    fn pop(&mut self) {
        self.invoke_stack.pop();
    }
    fn invoke_depth(&self) -> usize {
        self.invoke_stack.len()
    }
    fn verify(
        &mut self,
        _message: &Message,
        _instruction: &CompiledInstruction,
        _program_indices: &[usize],
    ) -> Result<(), InstructionError> {
        Ok(())
    }
    fn verify_and_update(
        &mut self,
        _instruction: &CompiledInstruction,
        _account_indices: &[usize],
        _write_pivileges: &[bool],
    ) -> Result<(), InstructionError> {
        Ok(())
    }
    fn get_caller(&self) -> Result<&Pubkey, InstructionError> {
        self.invoke_stack
            .last()
            .map(|frame| &frame.key)
            .ok_or(InstructionError::CallDepth)
    }
    fn remove_first_keyed_account(&mut self) -> Result<(), InstructionError> {
        if !self.is_feature_active(&remove_native_loader::id()) {
            let stack_frame = &mut self
                .invoke_stack
                .last_mut()
                .ok_or(InstructionError::CallDepth)?;
            stack_frame.keyed_accounts_range.start =
                stack_frame.keyed_accounts_range.start.saturating_add(1);
        }
        Ok(())
    }
    fn get_keyed_accounts(&self) -> Result<&[KeyedAccount], InstructionError> {
        self.invoke_stack
            .last()
            .map(|frame| &frame.keyed_accounts[frame.keyed_accounts_range.clone()])
            .ok_or(InstructionError::CallDepth)
    }
    fn get_programs(&self) -> &[(Pubkey, ProcessInstructionWithContext)] {
        &self.programs
    }
    fn get_logger(&self) -> Rc<RefCell<dyn Logger>> {
        Rc::new(RefCell::new(self.logger.clone()))
    }
    #[allow(deprecated)]
    fn get_bpf_compute_budget(&self) -> &BpfComputeBudget {
        #[allow(deprecated)]
        &self.bpf_compute_budget
    }
    fn get_compute_meter(&self) -> Rc<RefCell<dyn ComputeMeter>> {
        Rc::new(RefCell::new(self.compute_meter.clone()))
    }
    fn add_executor(&self, _pubkey: &Pubkey, _executor: Arc<dyn Executor>) {}
    fn get_executor(&self, _pubkey: &Pubkey) -> Option<Arc<dyn Executor>> {
        None
    }
    fn set_instruction_index(&mut self, _instruction_index: usize) {}
    fn record_instruction(&self, _instruction: &Instruction) {}
    fn is_feature_active(&self, feature_id: &Pubkey) -> bool {
        !self.disabled_features.contains(feature_id)
    }
    fn get_account(&self, pubkey: &Pubkey) -> Option<(usize, Rc<RefCell<AccountSharedData>>)> {
        for (index, (key, account)) in self.accounts.iter().enumerate().rev() {
            if key == pubkey {
                return Some((index, account.clone()));
            }
        }
        None
    }
    fn update_timing(
        &mut self,
        _serialize_us: u64,
        _create_vm_us: u64,
        _execute_us: u64,
        _deserialize_us: u64,
    ) {
    }
    fn get_sysvar_data(&self, id: &Pubkey) -> Option<Rc<Vec<u8>>> {
        self.sysvars
            .iter()
            .find_map(|(key, sysvar)| if id == key { sysvar.clone() } else { None })
    }
    fn get_compute_budget(&self) -> &ComputeBudget {
        &self.compute_budget
    }
    fn get_blockhash(&self) -> &Hash {
        &self.blockhash
    }
    fn get_fee_calculator(&self) -> &FeeCalculator {
        &self.fee_calculator
    }
    fn set_return_data(&mut self, data: Vec<u8>) -> Result<(), InstructionError> {
        self.return_data = (*self.get_caller()?, data);
        Ok(())
    }
    fn get_return_data(&self) -> (Pubkey, &[u8]) {
        (self.return_data.0, &self.return_data.1)
    }
}
