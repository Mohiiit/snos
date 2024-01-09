use std::cell::{RefCell, RefMut};
use std::rc::Rc;
use std::vec::IntoIter;

use blockifier::block_context::BlockContext;
use blockifier::execution::call_info::CallInfo;
use blockifier::execution::entry_point_execution::CallResult;
use blockifier::transaction::objects::TransactionExecutionInfo;
use cairo_vm::types::relocatable::Relocatable;
use cairo_vm::Felt252;
use starknet_api::deprecated_contract_class::EntryPointType;

use crate::config::STORED_BLOCK_HASH_BUFFER;

#[derive(Clone)]
pub struct ExecutionHelper {
    // _storage_by_address: HashMap<Felt252, OsSingleStarknetStorage<H, S>>,
    pub prev_block_context: Option<BlockContext>,
    pub tx_execution_info_iter: IntoIter<TransactionExecutionInfo>,
    pub tx_execution_info: Option<TransactionExecutionInfo>,
    pub tx_info_ptr: Option<Relocatable>,
    pub call_execution_info_ptr: Option<Relocatable>,
    pub call_iter: IntoIter<CallInfo>,
    pub call_info: Option<CallInfo>,
    pub result_iter: IntoIter<CallResult>,
    deployed_contracts_iter: IntoIter<Felt252>,
    execute_code_read_iter: IntoIter<Felt252>,
}

#[derive(Clone)]
pub struct ExecutionHelperManager {
    pub execution_helper: Rc<RefCell<ExecutionHelper>>,
}

impl ExecutionHelperManager {
    pub fn new(tx_execution_infos: Vec<TransactionExecutionInfo>, block_context: &BlockContext) -> Self {
        // TODO: look this up in storage_commitment_tree
        let prev_block_context =
            block_context.block_number.0.checked_sub(STORED_BLOCK_HASH_BUFFER).map(|_| block_context.clone());

        Self {
            execution_helper: Rc::new(RefCell::new(ExecutionHelper {
                prev_block_context,
                tx_execution_info_iter: tx_execution_infos.into_iter(),
                tx_execution_info: None,
                tx_info_ptr: None,
                call_iter: vec![].into_iter(),
                call_execution_info_ptr: None,
                call_info: None,
                result_iter: vec![].into_iter(),
                deployed_contracts_iter: vec![].into_iter(),
                execute_code_read_iter: vec![].into_iter(),
            })),
        }
    }
    pub fn start_tx(&self, tx_info_ptr: Option<Relocatable>) {
        println!("start tx...");
        let mut eh_ref = self.execution_helper.as_ref().borrow_mut();
        assert!(eh_ref.tx_info_ptr.is_none());
        eh_ref.tx_info_ptr = tx_info_ptr;
        assert!(eh_ref.tx_execution_info.is_none());
        eh_ref.tx_execution_info = eh_ref.tx_execution_info_iter.next();
        eh_ref.call_iter = eh_ref.tx_execution_info.as_ref().unwrap().gen_call_iterator();
    }
    pub fn end_tx(&self) {
        println!("end tx...");
        let mut eh_ref = self.execution_helper.as_ref().borrow_mut();
        assert!(eh_ref.call_iter.clone().peekable().peek().is_none());
        eh_ref.tx_info_ptr = None;
        assert!(eh_ref.tx_execution_info.is_some());
        eh_ref.tx_execution_info = None;
    }
    pub fn skip_tx(&self) {
        self.start_tx(None);
        self.end_tx()
    }
    pub fn enter_call(&self, execution_info_ptr: Option<Relocatable>) {
        println!("entered call...");
        let mut eh_ref = self.execution_helper.as_ref().borrow_mut();
        assert!(eh_ref.call_execution_info_ptr.is_none());
        eh_ref.call_execution_info_ptr = execution_info_ptr;

        assert_iterators_exhausted(&eh_ref);

        assert!(eh_ref.call_info.is_none());
        let call_info = eh_ref.call_iter.next().unwrap();

        // unpack deployed calls
        eh_ref.deployed_contracts_iter = call_info
            .inner_calls
            .iter()
            .filter_map(|call| {
                if matches!(call.call.entry_point_type, EntryPointType::Constructor) {
                    Some(Felt252::from_bytes_be_slice(call.call.caller_address.0.key().bytes()))
                } else {
                    None
                }
            })
            .collect::<Vec<Felt252>>()
            .into_iter();

        // unpack call results
        eh_ref.result_iter = call_info
            .inner_calls
            .iter()
            .map(|call| CallResult {
                failed: call.execution.failed,
                retdata: call.execution.retdata.clone(),
                gas_consumed: call.execution.gas_consumed,
            })
            .collect::<Vec<CallResult>>()
            .into_iter();

        // unpack storage reads
        eh_ref.execute_code_read_iter = call_info
            .storage_read_values
            .iter()
            .map(|felt| Felt252::from_bytes_be_slice(felt.bytes()))
            .collect::<Vec<Felt252>>()
            .into_iter();

        eh_ref.call_info = Some(call_info);
    }
    pub fn exit_call(&mut self) {
        println!("exit call...");
        let mut eh_ref = self.execution_helper.as_ref().borrow_mut();
        eh_ref.call_execution_info_ptr = None;
        assert_iterators_exhausted(&eh_ref);
        assert!(eh_ref.call_info.is_some());
        eh_ref.call_info = None;
    }
    pub fn skip_call(&mut self) {
        println!("skip call...");
        self.enter_call(None);
        self.exit_call();
    }
}

fn assert_iterators_exhausted(eh_ref: &RefMut<'_, ExecutionHelper>) {
    assert!(eh_ref.deployed_contracts_iter.clone().peekable().peek().is_none());
    assert!(eh_ref.result_iter.clone().peekable().peek().is_none());
    assert!(eh_ref.execute_code_read_iter.clone().peekable().peek().is_none());
}

trait GenCallIter {
    fn gen_call_iterator(&self) -> IntoIter<CallInfo>;
}
impl GenCallIter for TransactionExecutionInfo {
    fn gen_call_iterator(&self) -> IntoIter<CallInfo> {
        let mut call_infos = vec![];
        for call_info in self.non_optional_call_infos() {
            call_infos.extend(call_info.clone().gen_call_topology());
        }
        call_infos.into_iter()
    }
}

trait GenCallTopology {
    fn gen_call_topology(self) -> IntoIter<CallInfo>;
}

impl GenCallTopology for CallInfo {
    fn gen_call_topology(self) -> IntoIter<CallInfo> {
        // Create a vector to store the results
        let mut results = vec![self.clone()];

        // Iterate over internal calls, recursively call gen_call_topology, and collect the results
        for call in self.inner_calls.into_iter() {
            results.extend(call.gen_call_topology());
        }

        // Convert the results vector into an iterator and return it
        results.into_iter()
    }
}
