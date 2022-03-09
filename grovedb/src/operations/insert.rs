use merk::Merk;
use storage::{Storage, StorageContext};

use crate::{
    util::{merk_optional_tx, meta_storage_context_optional_tx},
    Element, Error, GroveDb, TransactionArg, ROOT_LEAFS_SERIALIZED_KEY,
};

impl GroveDb {
    pub fn insert<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element,
        transaction: TransactionArg,
    ) -> Result<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let path_iter = path.into_iter();
        match element {
            Element::Tree(_) => {
                if path_iter.len() == 0 {
                    self.add_root_leaf(key, transaction)?;
                } else {
                    self.add_non_root_subtree(path_iter, key, transaction)?;
                }
            }
            _ => {
                // If path is empty that means there is an attempt to insert
                // something into a root tree and this branch is for anything
                // but trees
                if path_iter.len() == 0 {
                    return Err(Error::InvalidPath(
                        "only subtrees are allowed as root tree's leafs",
                    ));
                }
                self.check_subtree_exists_invalid_path(path_iter.clone(), transaction)?;
                merk_optional_tx!(self.db, path_iter.clone(), transaction, mut subtree, {
                    element.insert(&mut subtree, key)?;
                });
                self.propagate_changes(path_iter, transaction)?;
            }
        }
        Ok(())
    }

    /// Add subtree to the root tree
    fn add_root_leaf(&self, key: &[u8], transaction: TransactionArg) -> Result<(), Error> {
        meta_storage_context_optional_tx!(self.db, transaction, meta_storage, {
            let mut root_leaf_keys = Self::get_root_leaf_keys_internal(&meta_storage)?;
            if root_leaf_keys.get(&key.to_vec()).is_none() {
                root_leaf_keys.insert(key.to_vec(), root_leaf_keys.len());
            }
            let value = bincode::serialize(&root_leaf_keys).map_err(|_| {
                Error::CorruptedData(String::from("unable to serialize root leaves data"))
            })?;
            meta_storage.put_meta(ROOT_LEAFS_SERIALIZED_KEY, &value)?;
        });

        Ok(())
    }

    /// Add subtree to another subtree.
    /// We want to add a new empty merk to another merk at a key
    /// first make sure other merk exist
    /// if it exists, then create merk to be inserted, and get root hash
    /// we only care about root hash of merk to be inserted
    fn add_non_root_subtree<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> Result<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let path_iter = path.into_iter();
        self.check_subtree_exists_invalid_path(path_iter.clone(), transaction)?;
        if let Some(tx) = transaction {
            let parent_storage = self
                .db
                .get_transactional_storage_context(path_iter.clone(), tx);
            let mut parent_subtree = Merk::open(parent_storage)
                .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))?;
            let child_storage = self.db.get_transactional_storage_context(
                path_iter.clone().chain(std::iter::once(key)),
                tx,
            );
            let child_subtree = Merk::open(child_storage)
                .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))?;
            let element = Element::Tree(child_subtree.root_hash());
            element.insert(&mut parent_subtree, key)?;
        } else {
            let parent_storage = self.db.get_storage_context(path_iter.clone());
            let mut parent_subtree = Merk::open(parent_storage)
                .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))?;
            let child_storage = self
                .db
                .get_storage_context(path_iter.clone().chain(std::iter::once(key)));
            let child_subtree = Merk::open(child_storage)
                .map_err(|_| crate::Error::CorruptedData("cannot open a subtree".to_owned()))?;
            let element = Element::Tree(child_subtree.root_hash());
            element.insert(&mut parent_subtree, key)?;
        }
        self.propagate_changes(path_iter, transaction)?;
        Ok(())
    }

    pub fn insert_if_not_exists<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        element: Element,
        transaction: TransactionArg,
    ) -> Result<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let path_iter = path.into_iter();
        if self.get_raw(path_iter.clone(), key, transaction).is_ok() {
            Ok(false)
        } else {
            match self.insert(path_iter, key, element, transaction) {
                Ok(_) => Ok(true),
                Err(e) => Err(e),
            }
        }
    }
}
