//! Impementation for a storage abstraction over RocksDB.
use std::path::Path;

use lazy_static::lazy_static;
use rocksdb::{ColumnFamilyDescriptor, Error, OptimisticTransactionDB, Transaction};

use super::{PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext};
use crate::Storage;

/// Name of column family used to store auxiliary data
pub(super) const AUX_CF_NAME: &str = "aux";
/// Name of column family used to store subtrees roots data
pub(super) const ROOTS_CF_NAME: &str = "roots";
/// Name of column family used to store metadata
pub(super) const META_CF_NAME: &str = "meta";

lazy_static! {
    static ref DEFAULT_OPTS: rocksdb::Options = {
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        opts.increase_parallelism(num_cpus::get() as i32);
        opts.set_allow_mmap_writes(true);
        opts.set_allow_mmap_reads(true);
        opts.create_missing_column_families(true);
        opts.set_atomic_flush(true);
        opts
    };
}

/// Storage which uses RocksDB as its backend.
pub struct RocksDbStorage {
    db: OptimisticTransactionDB,
}

impl RocksDbStorage {
    pub fn default_rocksdb_with_path<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let db = rocksdb::OptimisticTransactionDB::open_cf_descriptors(
            &DEFAULT_OPTS,
            &path,
            [
                ColumnFamilyDescriptor::new(AUX_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(ROOTS_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(META_CF_NAME, DEFAULT_OPTS.clone()),
            ],
        )?;

        Ok(RocksDbStorage { db })
    }

    pub fn get_prefixed_context(&self, prefix: Vec<u8>) -> PrefixedRocksDbStorageContext {
        PrefixedRocksDbStorageContext::new(&self.db, prefix)
    }

    pub fn get_prefixed_transactional_context<'a>(
        &'a self,
        prefix: Vec<u8>,
        transaction: &'a <Self as Storage>::Transaction,
    ) -> PrefixedRocksDbTransactionContext {
        PrefixedRocksDbTransactionContext::new(&self.db, transaction, prefix)
    }
}

impl<'db> Storage<'db> for RocksDbStorage {
    type Error = Error;
    type Transaction = Transaction<'db, OptimisticTransactionDB>;

    fn start_transaction(&'db self) -> Self::Transaction {
        self.db.transaction()
    }

    fn commit_transaction(&self, transaction: Self::Transaction) -> Result<(), Self::Error> {
        transaction.commit()
    }

    fn rollback_transaction(&self, transaction: &Self::Transaction) -> Result<(), Self::Error> {
        transaction.rollback()
    }

    fn flush(&self) -> Result<(), Self::Error> {
        self.db.flush()
    }
}
