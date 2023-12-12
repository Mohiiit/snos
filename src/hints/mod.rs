pub mod block_context;
mod execution;
pub mod hints_raw;
// pub mod transaction_context;

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::vec::IntoIter;

use cairo_vm::felt::Felt252;
use cairo_vm::hint_processor::builtin_hint_processor::builtin_hint_processor_definition::{
    BuiltinHintProcessor, HintFunc,
};
use cairo_vm::hint_processor::builtin_hint_processor::hint_utils::*;
use cairo_vm::hint_processor::hint_processor_definition::HintReference;
use cairo_vm::serde::deserialize_program::ApTracking;
use cairo_vm::types::exec_scope::ExecutionScopes;
use cairo_vm::types::relocatable::MaybeRelocatable;
use cairo_vm::vm::errors::hint_errors::HintError;
use cairo_vm::vm::vm_core::VirtualMachine;

use self::block_context::{get_block_mapping, load_class_facts, load_deprecated_inner, load_deprecated_class_facts, block_number, block_timestamp, sequencer_address, chain_id, fee_token_address};
use self::execution::{
    check_is_deprecated, enter_call, get_state_entry, is_deprecated, os_context_segments, select_builtin,
    selected_builtins, start_execute_deploy_transaction,
};
use crate::config::DEFAULT_INPUT_PATH;
use crate::hints::hints_raw::*;
use crate::io::execution_helper::OsExecutionHelper;
use crate::io::input::StarknetOsInput;
use crate::io::InternalTransaction;
use crate::state::storage::TrieStorage;
use crate::state::trie::PedersenHash;

pub fn sn_hint_processor() -> BuiltinHintProcessor {
    let hints: Box<[(
        &str, 
        fn(
            &mut VirtualMachine,
            &mut ExecutionScopes,
            &HashMap<String, HintReference>,
            &ApTracking,
            &HashMap<String, Felt252>,
        ) -> Result<(), HintError>)]> =
    Box::new([
        (ASSERT_TRANSACTION_HASH, assert_transaction_hash),
        (CHAIN_ID, chain_id),
        (CHECK_DEPRECATED_CLASS_HASH, check_deprecated_class_hash),
        (CHECK_IS_DEPRECATED, check_is_deprecated),
        (DEPRECATED_BLOCK_NUMBER, block_number),
        (DEPRECATED_BLOCK_TIMESTAMP, block_timestamp),
        (ENTER_CALL, enter_call),
        (ENTER_SYSCALL_SCOPES, enter_syscall_scopes),
        (FEE_TOKEN_ADDRESS, fee_token_address),
        (GET_BLOCK_MAPPING, get_block_mapping),
        (GET_STATE_ENTRY, get_state_entry),
        (INITIALIZE_CLASS_HASHES, initialize_class_hashes),
        (INITIALIZE_STATE_CHANGES, initialize_state_changes),
        (IS_DEPRECATED, is_deprecated),
        (LOAD_CLASS_FACTS, load_class_facts),
        (LOAD_DEPRECATED_CLASS_FACTS, load_deprecated_class_facts),
        (LOAD_DEPRECATED_CLASS_INNER, load_deprecated_inner),
        (LOAD_NEXT_TX, load_next_tx),
        (OS_CONTEXT_SEGMENTS, os_context_segments),
        (PREPARE_CONSTRUCTOR_EXECUTION, prepare_constructor_execution),
        (SEGMENTS_ADD_TEMP, segments_add_temp),
        (SEGMENTS_ADD, segments_add),
        (SELECT_BUILTIN, select_builtin),
        (SELECTED_BUILTINS, selected_builtins),
        (SEQUENCER_ADDRESS, sequencer_address),
        (STARKNET_OS_INPUT, starknet_os_input),
        (START_DEPLOY_TX, start_execute_deploy_transaction),
        (TRANSACTION_VERSION, transaction_version),
        (TRANSACTIONS_LEN, transactions_len),
    ]);

    let mut hint_processor = BuiltinHintProcessor::new_empty();

    for (hint_name, hint_func) in hints.iter() {
        let sn_hint = HintFunc(Box::new(*hint_func));
        hint_processor.add_hint(String::from(*hint_name), Rc::new(sn_hint));
    }

    hint_processor
}

/// Implements hint:
///
/// from starkware.starknet.core.os.os_input import StarknetOsInput
///
/// os_input = StarknetOsInput.load(data=program_input)
///
/// ids.initial_carried_outputs.messages_to_l1 = segments.add_temp_segment()
/// ids.initial_carried_outputs.messages_to_l2 = segments.add_temp_segment()
pub fn starknet_os_input(
    vm: &mut VirtualMachine,
    exec_scopes: &mut ExecutionScopes,
    ids_data: &HashMap<String, HintReference>,
    ap_tracking: &ApTracking,
    _constants: &HashMap<String, Felt252>,
) -> Result<(), HintError> {
    let input_path =
        std::path::PathBuf::from(exec_scopes.get::<String>("input_path").unwrap_or(DEFAULT_INPUT_PATH.to_string()));

    let os_input = Box::new(
        StarknetOsInput::load(&input_path).map_err(|e| HintError::CustomHint(e.to_string().into_boxed_str()))?,
    );
    exec_scopes.assign_or_update_variable("os_input", os_input);

    let initial_carried_outputs_ptr = get_ptr_from_var_name("initial_carried_outputs", vm, ids_data, ap_tracking)?;

    let messages_to_l1 = initial_carried_outputs_ptr;
    let temp_segment = vm.add_temporary_segment();
    vm.insert_value(messages_to_l1, temp_segment)?;

    let messages_to_l2 = (initial_carried_outputs_ptr + 1_i32)?;
    let temp_segment = vm.add_temporary_segment();
    vm.insert_value(messages_to_l2, temp_segment).map_err(|e| e.into())
}

/// Implements hint:
///
/// from starkware.python.utils import from_bytes
///
/// computed_hash = ids.compiled_class_fact.hash
/// expected_hash = compiled_class_hash
/// assert computed_hash == expected_hash, (
/// "Computed compiled_class_hash is inconsistent with the hash in the os_input. "
/// f"Computed hash = {computed_hash}, Expected hash = {expected_hash}.")
///
/// vm_load_program(compiled_class.program, ids.compiled_class.bytecode_ptr)
pub fn check_deprecated_class_hash(
    _vm: &mut VirtualMachine,
    _exec_scopes: &mut ExecutionScopes,
    _ids_data: &HashMap<String, HintReference>,
    _ap_tracking: &ApTracking,
    _constants: &HashMap<String, Felt252>,
) -> Result<(), HintError> {
    // TODO: decide if we really need to check this deprecated hash moving forward
    // TODO: check w/ LC for `vm_load_program` impl

    Ok(())
}

/// Implements hint:
///
/// from starkware.python.utils import from_bytes
///
/// initial_dict = {
///     address: segments.gen_arg(
///         (from_bytes(contract.contract_hash), segments.add(), contract.nonce))
///     for address, contract in os_input.contracts.items()
/// }
pub fn initialize_state_changes(
    vm: &mut VirtualMachine,
    exec_scopes: &mut ExecutionScopes,
    _ids_data: &HashMap<String, HintReference>,
    _ap_tracking: &ApTracking,
    _constants: &HashMap<String, Felt252>,
) -> Result<(), HintError> {
    let os_input = exec_scopes.get::<StarknetOsInput>("os_input")?;
    let mut state_dict: HashMap<MaybeRelocatable, MaybeRelocatable> = HashMap::new();
    for (addr, contract_state) in os_input.contracts {
        let change_base = vm.add_memory_segment();
        vm.insert_value(change_base, contract_state.contract_hash)?;
        let storage_commitment_base = vm.add_memory_segment();
        vm.insert_value((change_base + 1)?, storage_commitment_base)?;
        vm.insert_value((change_base + 2)?, contract_state.nonce)?;

        state_dict.insert(MaybeRelocatable::from(addr), MaybeRelocatable::from(change_base));
    }

    exec_scopes.insert_box("initial_dict", Box::new(state_dict));
    Ok(())
}

/// Implements hint:
///
/// initial_dict = os_input.class_hash_to_compiled_class_hash
pub fn initialize_class_hashes(
    _vm: &mut VirtualMachine,
    exec_scopes: &mut ExecutionScopes,
    _ids_data: &HashMap<String, HintReference>,
    _ap_tracking: &ApTracking,
    _constants: &HashMap<String, Felt252>,
) -> Result<(), HintError> {
    let os_input = exec_scopes.get::<StarknetOsInput>("os_input")?;
    let mut class_dict: HashMap<MaybeRelocatable, MaybeRelocatable> = HashMap::new();
    for (class_hash, compiled_class_hash) in os_input.class_hash_to_compiled_class_hash {
        class_dict.insert(MaybeRelocatable::from(class_hash), MaybeRelocatable::from(compiled_class_hash));
    }

    exec_scopes.insert_box("initial_dict", Box::new(class_dict));
    Ok(())
}

/// Implements hint:
///
/// memory[ap] = to_felt_or_relocatable(segments.add())
pub fn segments_add(
    vm: &mut VirtualMachine,
    _exec_scopes: &mut ExecutionScopes,
    _ids_data: &HashMap<String, HintReference>,
    _ap_tracking: &ApTracking,
    _constants: &HashMap<String, Felt252>,
) -> Result<(), HintError> {
    let segment = vm.add_memory_segment();
    insert_value_into_ap(vm, segment)
}

/// Implements hint:
///
/// memory[ap] = to_felt_or_relocatable(segments.add_temp_segment())
pub fn segments_add_temp(
    vm: &mut VirtualMachine,
    _exec_scopes: &mut ExecutionScopes,
    _ids_data: &HashMap<String, HintReference>,
    _ap_tracking: &ApTracking,
    _constants: &HashMap<String, Felt252>,
) -> Result<(), HintError> {
    let temp_segment = vm.add_temporary_segment();
    insert_value_into_ap(vm, temp_segment)
}

/// Implements hint:
///
/// memory[ap] = to_felt_or_relocatable(len(os_input.transactions))
pub fn transactions_len(
    vm: &mut VirtualMachine,
    exec_scopes: &mut ExecutionScopes,
    _ids_data: &HashMap<String, HintReference>,
    _ap_tracking: &ApTracking,
    _constants: &HashMap<String, Felt252>,
) -> Result<(), HintError> {
    let os_input = exec_scopes.get::<StarknetOsInput>("os_input")?;

    insert_value_into_ap(vm, os_input.transactions.len())
}

/// Implements hint:
///
/// vm_enter_scope({
///     '__deprecated_class_hashes': __deprecated_class_hashes,
///     'transactions': iter(os_input.transactions),
///     'execution_helper': execution_helper,
///     'deprecated_syscall_handler': deprecated_syscall_handler,
///     'syscall_handler': syscall_handler,
///      '__dict_manager': __dict_manager,
/// })
pub fn enter_syscall_scopes(
    _vm: &mut VirtualMachine,
    exec_scopes: &mut ExecutionScopes,
    _ids_data: &HashMap<String, HintReference>,
    _ap_tracking: &ApTracking,
    _constants: &HashMap<String, Felt252>,
) -> Result<(), HintError> {
    let os_input = exec_scopes.get::<StarknetOsInput>("os_input").unwrap();
    let transactions: Box<dyn Any> = Box::new(os_input.transactions.into_iter());
    let dict_manager = Box::new(exec_scopes.get_dict_manager()?);
    let deprecated_class_hashes = Box::new(exec_scopes.get::<HashSet<Felt252>>("__deprecated_class_hashes")?);
    let execution_helper =
        Box::new(exec_scopes.get::<OsExecutionHelper<PedersenHash, TrieStorage>>("execution_helper")?);
    exec_scopes.enter_scope(HashMap::from_iter([
        (String::from("transactions"), transactions),
        (String::from("execution_helper"), execution_helper),
        (String::from("dict_manager"), dict_manager),
        (String::from("__deprecated_class_hashes"), deprecated_class_hashes),
    ]));
    Ok(())
}

/// Implements hint:
///
/// tx = next(transactions)
/// tx_type_bytes = tx.tx_type.name.encode("ascii")
/// ids.tx_type = int.from_bytes(tx_type_bytes, "big")
pub fn load_next_tx(
    vm: &mut VirtualMachine,
    exec_scopes: &mut ExecutionScopes,
    ids_data: &HashMap<String, HintReference>,
    ap_tracking: &ApTracking,
    _constants: &HashMap<String, Felt252>,
) -> Result<(), HintError> {
    let mut transactions = exec_scopes.get::<IntoIter<InternalTransaction>>("transactions")?;
    // Safe to unwrap because the remaining number of txs is checked in the cairo code.
    let tx = transactions.next().unwrap();
    exec_scopes.insert_value("transactions", transactions);
    exec_scopes.insert_value("tx", tx.clone());
    insert_value_from_var_name("tx_type", Felt252::from_bytes_be(tx.r#type.as_bytes()), vm, ids_data, ap_tracking)
}

/// Implements hint:
///
/// ids.contract_address_salt = tx.contract_address_salt
/// ids.class_hash = tx.class_hash
/// ids.constructor_calldata_size = len(tx.constructor_calldata)
/// ids.constructor_calldata = segments.gen_arg(arg=tx.constructor_calldata)
pub fn prepare_constructor_execution(
    vm: &mut VirtualMachine,
    exec_scopes: &mut ExecutionScopes,
    ids_data: &HashMap<String, HintReference>,
    ap_tracking: &ApTracking,
    _constants: &HashMap<String, Felt252>,
) -> Result<(), HintError> {
    let tx = exec_scopes.get::<InternalTransaction>("tx")?;
    insert_value_from_var_name(
        "contract_address_salt",
        tx.contract_address_salt.expect("`contract_address_salt` must be present"),
        vm,
        ids_data,
        ap_tracking,
    )?;
    insert_value_from_var_name(
        "class_hash",
        // using `contract_hash` instead of `class_hash` as the that's how the
        // input.json is structured
        tx.contract_hash.expect("`contract_hash` must be present"),
        vm,
        ids_data,
        ap_tracking,
    )?;

    let constructor_calldata_size = match &tx.constructor_calldata {
        None => 0,
        Some(calldata) => calldata.len(),
    };
    insert_value_from_var_name("constructor_calldata_size", constructor_calldata_size, vm, ids_data, ap_tracking)?;

    let constructor_calldata = tx.constructor_calldata.unwrap_or_default().iter().map(|felt| felt.into()).collect();
    let constructor_calldata_base = vm.add_memory_segment();
    vm.load_data(constructor_calldata_base, &constructor_calldata)?;
    insert_value_from_var_name("constructor_calldata", constructor_calldata_base, vm, ids_data, ap_tracking)
}

/// Implements hint:
///
/// memory[ap] = to_felt_or_relocatable(tx.version)
pub fn transaction_version(
    vm: &mut VirtualMachine,
    exec_scopes: &mut ExecutionScopes,
    _ids_data: &HashMap<String, HintReference>,
    _ap_tracking: &ApTracking,
    _constants: &HashMap<String, Felt252>,
) -> Result<(), HintError> {
    let tx = exec_scopes.get::<InternalTransaction>("tx")?;
    insert_value_into_ap(vm, tx.version.expect("Transaction version should be set"))
}

/// Implements hint:
///
/// assert ids.transaction_hash == tx.hash_value, (
/// "Computed transaction_hash is inconsistent with the hash in the transaction. "
/// f"Computed hash = {ids.transaction_hash}, Expected hash = {tx.hash_value}.")
pub fn assert_transaction_hash(
    vm: &mut VirtualMachine,
    exec_scopes: &mut ExecutionScopes,
    ids_data: &HashMap<String, HintReference>,
    ap_tracking: &ApTracking,
    _constants: &HashMap<String, Felt252>,
) -> Result<(), HintError> {
    let tx = exec_scopes.get::<InternalTransaction>("tx")?;
    let transaction_hash = get_integer_from_var_name("transaction_hash", vm, ids_data, ap_tracking)?.into_owned();

    assert_eq!(
        tx.hash_value, transaction_hash,
        "Computed transaction_hash is inconsistent with the hash in the transaction. Computed hash = {}, Expected \
         hash = {}.",
        transaction_hash, tx.hash_value
    );
    Ok(())
}
