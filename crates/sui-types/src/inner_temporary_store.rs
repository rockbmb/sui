// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::base_types::{SequenceNumber, VersionDigest};
use crate::effects::{TransactionEffects, TransactionEffectsAPI, TransactionEvents};
use crate::execution::DynamicallyLoadedObjectMetadata;
use crate::storage::InputKey;
use crate::storage::PackageObjectArc;
use crate::{
    base_types::ObjectID,
    object::{Object, Owner},
};
use move_binary_format::CompiledModule;
use move_bytecode_utils::module_cache::GetModule;
use move_core_types::language_storage::ModuleId;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

pub type WrittenObjects = BTreeMap<ObjectID, Object>;
pub type ObjectMap = BTreeMap<ObjectID, Arc<Object>>;
pub type TxCoins = (ObjectMap, WrittenObjects);

#[derive(Debug, Clone)]
pub struct InnerTemporaryStore {
    pub input_objects: ObjectMap,
    pub mutable_inputs: BTreeMap<ObjectID, (VersionDigest, Owner)>,
    // All the written objects' sequence number should have been updated to the lamport version.
    pub written: WrittenObjects,
    pub loaded_runtime_objects: BTreeMap<ObjectID, DynamicallyLoadedObjectMetadata>,
    pub events: TransactionEvents,
    pub max_binary_format_version: u32,
    pub no_extraneous_module_bytes: bool,
    pub runtime_packages_loaded_from_db: BTreeMap<ObjectID, PackageObjectArc>,
    pub lamport_version: SequenceNumber,
}

impl InnerTemporaryStore {
    pub fn get_output_keys(&self, effects: &TransactionEffects) -> Vec<InputKey> {
        let mut output_keys: Vec<_> = self
            .written
            .iter()
            .map(|(id, obj)| {
                if obj.is_package() {
                    InputKey::Package { id: *id }
                } else {
                    InputKey::VersionedObject {
                        id: *id,
                        version: obj.version(),
                    }
                }
            })
            .collect();

        let deleted: HashMap<_, _> = effects
            .deleted()
            .iter()
            .map(|oref| (oref.0, oref.1))
            .collect();

        // add deleted shared objects to the outputkeys that then get sent to notify_commit
        let deleted_output_keys = deleted
            .iter()
            .filter(|(id, _)| {
                self.input_objects
                    .get(id)
                    .is_some_and(|obj| obj.is_shared())
            })
            .map(|(id, seq)| InputKey::VersionedObject {
                id: *id,
                version: *seq,
            });
        output_keys.extend(deleted_output_keys);

        // For any previously deleted shared objects that appeared mutably in the transaction,
        // synthesize a notification for the next version of the object.
        let smeared_version = self.lamport_version;
        let deleted_accessed_objects = effects.deleted_mutably_accessed_shared_objects();
        for object_id in deleted_accessed_objects.into_iter() {
            let key = InputKey::VersionedObject {
                id: object_id,
                version: smeared_version,
            };
            output_keys.push(key);
        }

        output_keys
    }
}

pub struct TemporaryModuleResolver<'a, R> {
    temp_store: &'a InnerTemporaryStore,
    fallback: R,
}

impl<'a, R> TemporaryModuleResolver<'a, R> {
    pub fn new(temp_store: &'a InnerTemporaryStore, fallback: R) -> Self {
        Self {
            temp_store,
            fallback,
        }
    }
}

impl<R> GetModule for TemporaryModuleResolver<'_, R>
where
    R: GetModule<Item = Arc<CompiledModule>, Error = anyhow::Error>,
{
    type Error = anyhow::Error;
    type Item = Arc<CompiledModule>;

    fn get_module_by_id(&self, id: &ModuleId) -> anyhow::Result<Option<Self::Item>, Self::Error> {
        let obj = self.temp_store.written.get(&ObjectID::from(*id.address()));
        if let Some(o) = obj {
            if let Some(p) = o.data.try_as_package() {
                return Ok(Some(Arc::new(p.deserialize_module(
                    &id.name().into(),
                    self.temp_store.max_binary_format_version,
                    self.temp_store.no_extraneous_module_bytes,
                )?)));
            }
        }
        self.fallback.get_module_by_id(id)
    }
}
