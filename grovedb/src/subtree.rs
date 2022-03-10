//! Module for subtrees handling.
//! Subtrees handling is isolated so basically this module is about adapting
//! Merk API to GroveDB needs.

use merk::{
    proofs::{query::QueryItem, Query},
    tree::Tree,
    Op,
};
use serde::{Deserialize, Serialize};
use storage::{
    rocksdb_storage::{
        OptimisticTransactionDBTransaction, PrefixedRocksDbStorage,
        RawPrefixedTransactionalIterator,
    },
    RawIterator, Storage, Store,
};

use crate::{Error, Merk, PathQuery, SizedQuery, Subtrees};

/// Variants of GroveDB stored entities
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Element {
    /// An ordinary value
    Item(Vec<u8>),
    /// A reference to an object by its path
    Reference(Vec<Vec<u8>>),
    /// A subtree, contains a root hash of the underlying Merk.
    /// Hash is stored to make Merk become different when its subtrees have
    /// changed, otherwise changes won't be reflected in parent trees.
    Tree([u8; 32]),
}

pub struct PathQueryPushArgs<'a> {
    pub transaction: Option<&'a OptimisticTransactionDBTransaction<'a>>,
    pub subtrees: &'a Subtrees<'a>,
    pub key: Option<&'a [u8]>,
    pub element: Element,
    pub path: Option<&'a [&'a [u8]]>,
    pub subquery_key: Option<Vec<u8>>,
    pub subquery: Option<Query>,
    pub left_to_right: bool,
    pub results: &'a mut Vec<Element>,
    pub limit: &'a mut Option<u16>,
    pub offset: &'a mut Option<u16>,
}

impl Element {
    // TODO: improve API to avoid creation of Tree elements with uncertain state
    pub fn empty_tree() -> Self {
        Self::Tree(Default::default())
    }

    /// Delete an element from Merk under a key
    pub fn delete<K: AsRef<[u8]>>(
        merk: &mut Merk<PrefixedRocksDbStorage>,
        key: K,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<(), Error> {
        // TODO: delete references on this element
        let batch = [(key, Op::Delete)];
        merk.apply::<_, Vec<u8>>(&batch, &[], transaction)
            .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    /// Get an element from Merk under a key; path should be resolved and proper
    /// Merk should be loaded by this moment
    pub fn get<K: AsRef<[u8]>>(merk: &Merk<PrefixedRocksDbStorage>, key: K) -> Result<Self, Error> {
        let element = bincode::deserialize(
            merk.get(key.as_ref())
                .map_err(|e| Error::CorruptedData(e.to_string()))?
                .ok_or_else(|| {
                    Error::PathKeyNotFound(format!("key not found in Merk: {}", hex::encode(key)))
                })?
                .as_slice(),
        )
        .map_err(|_| Error::CorruptedData(String::from("unable to deserialize element")))?;
        Ok(element)
    }

    pub fn get_query(
        merk_path: &[&[u8]],
        query: &Query,
        transaction: Option<&OptimisticTransactionDBTransaction>,
        subtrees: &Subtrees,
    ) -> Result<Vec<Self>, Error> {
        let sized_query = SizedQuery::new(query.clone(), None, None);
        let (elements, _) = Self::get_sized_query(merk_path, &sized_query, transaction, subtrees)?;
        Ok(elements)
    }

    fn basic_push(args: PathQueryPushArgs) -> Result<(), Error> {
        let PathQueryPushArgs {
            element,
            results,
            limit,
            offset,
            ..
        } = args;
        if offset.unwrap_or(0) == 0 {
            results.push(element);
            if let Some(limit) = limit {
                *limit -= 1;
            }
        } else if let Some(offset) = offset {
            *offset -= 1;
        }
        Ok(())
    }

    fn path_query_push(args: PathQueryPushArgs) -> Result<(), Error> {
        let PathQueryPushArgs {
            transaction,
            subtrees,
            key,
            element,
            path,
            subquery_key,
            subquery,
            left_to_right,
            results,
            limit,
            offset,
        } = args;
        match element {
            Element::Tree(_) => {
                // if subquery.is_none() {
                //     return Err(Error::InvalidPath(
                //         "a subtrees_option or a subquery should be provided",
                //     ));
                // }
                // this means that for each element we should get the element at
                // the subquery_key or just the directly with the subquery
                let mut path_vec = path
                    .ok_or(Error::MissingParameter(
                        "the path must be provided when using a subquery key",
                    ))?
                    .to_vec();
                path_vec.push(key.ok_or(Error::MissingParameter(
                    "the key must be provided when using a subquery key",
                ))?);

                if let Some(subquery) = subquery {
                    if let Some(subquery_key) = &subquery_key {
                        path_vec.push(subquery_key.as_slice());
                    }

                    let inner_query = SizedQuery::new(subquery, *limit, *offset);
                    let path_vec_owned = path_vec.iter().map(|x| x.to_vec()).collect();
                    let inner_path_query = PathQuery::new(path_vec_owned, inner_query);

                    let (mut sub_elements, skipped) =
                        Self::get_path_query(&path_vec, &inner_path_query, transaction, subtrees)?;

                    if let Some(limit) = limit {
                        *limit -= sub_elements.len() as u16;
                    }
                    if let Some(offset) = offset {
                        *offset -= skipped;
                    }
                    results.append(&mut sub_elements);
                } else if let Some(subquery_key) = subquery_key {
                    if offset.unwrap_or(0) == 0 {
                        let element = subtrees
                            .borrow_mut(path_vec.iter().copied(), transaction)?
                            .apply(|s| Self::get(s, subquery_key.as_slice()))?;
                        results.push(element);
                        if let Some(limit) = limit {
                            *limit -= 1;
                        }
                    } else if let Some(offset) = offset {
                        *offset -= 1;
                    }
                } else {
                    return Err(Error::InvalidPath(
                        "you must provide a subquery or a subquery_key when interacting with a \
                         tree of trees",
                    ));
                }
            }
            _ => {
                Self::basic_push(PathQueryPushArgs {
                    transaction,
                    subtrees,
                    key,
                    element,
                    path,
                    subquery_key,
                    subquery,
                    left_to_right,
                    results,
                    limit,
                    offset,
                })?;
            }
        }
        Ok(())
    }

    fn query_item(
        item: &QueryItem,
        results: &mut Vec<Self>,
        merk_path: &[&[u8]],
        sized_query: &SizedQuery,
        path: Option<&[&[u8]]>,
        transaction: Option<&OptimisticTransactionDBTransaction>,
        subtrees: &Subtrees,
        limit: &mut Option<u16>,
        offset: &mut Option<u16>,
        add_element_function: fn(PathQueryPushArgs) -> Result<(), Error>,
    ) -> Result<(), Error> {
        if !item.is_range() {
            // this is a query on a key
            if let QueryItem::Key(key) = item {
                match subtrees
                    .borrow_mut(merk_path.iter().copied(), transaction)?
                    .apply(|s| Self::get(s, key))
                {
                    Ok(element) => Ok(add_element_function(PathQueryPushArgs {
                        transaction,
                        subtrees,
                        key: Some(key.as_slice()),
                        element,
                        path,
                        subquery_key: sized_query.query.subquery_key.clone(),
                        subquery: sized_query
                            .query
                            .subquery
                            .as_ref()
                            .map(|query| *query.clone()),
                        left_to_right: sized_query.query.left_to_right,
                        results,
                        limit,
                        offset,
                    })?),
                    Err(e) => match e {
                        Error::PathKeyNotFound(_) => Ok(()),
                        _ => Err(e),
                    },
                }
            } else {
                Err(Error::InternalError(
                    "QueryItem must be a Key if not a range",
                ))
            }
        } else {
            // this is a query on a range

            // TODO: no better way until storage refactoring
            let storage = subtrees
                .borrow_mut(merk_path.iter().copied(), transaction)?
                .apply(|s| s.storage.clone());

            let mut iter = storage.raw_iter(transaction);

            item.seek_for_iter(&mut iter, sized_query.query.left_to_right);

            while item.iter_is_valid_for_type(&iter, *limit, sized_query.query.left_to_right) {
                let element =
                    raw_decode(iter.value().expect("if key exists then value should too"))?;
                let key = iter.key().expect("key should exist");
                add_element_function(PathQueryPushArgs {
                    transaction,
                    subtrees,
                    key: Some(key),
                    element,
                    path,
                    subquery_key: sized_query.query.subquery_key.clone(),
                    subquery: sized_query
                        .query
                        .subquery
                        .as_ref()
                        .map(|query| *query.clone()),
                    left_to_right: sized_query.query.left_to_right,
                    results,
                    limit,
                    offset,
                })?;
                if sized_query.query.left_to_right {
                    iter.next();
                } else {
                    iter.prev();
                }
            }
            Ok(())
        }
    }

    pub fn get_query_apply_function(
        merk_path: &[&[u8]],
        sized_query: &SizedQuery,
        path: Option<&[&[u8]]>,
        transaction: Option<&OptimisticTransactionDBTransaction>,
        subtrees: &Subtrees,
        add_element_function: fn(PathQueryPushArgs) -> Result<(), Error>,
    ) -> Result<(Vec<Self>, u16), Error> {
        let mut results = Vec::new();

        let mut limit = sized_query.limit;
        let original_offset = sized_query.offset;
        let mut offset = original_offset;

        if sized_query.query.left_to_right {
            for item in sized_query.query.iter() {
                Self::query_item(
                    item,
                    &mut results,
                    merk_path,
                    sized_query,
                    path,
                    transaction,
                    subtrees,
                    &mut limit,
                    &mut offset,
                    add_element_function,
                )?;
                if limit == Some(0) {
                    break;
                }
            }
        } else {
            for item in sized_query.query.rev_iter() {
                Self::query_item(
                    item,
                    &mut results,
                    merk_path,
                    sized_query,
                    path,
                    transaction,
                    subtrees,
                    &mut limit,
                    &mut offset,
                    add_element_function,
                )?;
                if limit == Some(0) {
                    break;
                }
            }
        }

        let skipped = if let Some(original_offset_unwrapped) = original_offset {
            original_offset_unwrapped - offset.unwrap()
        } else {
            0
        };
        Ok((results, skipped))
    }

    // Returns a vector of elements, and the number of skipped elements
    pub fn get_path_query(
        merk_path: &[&[u8]],
        path_query: &PathQuery,
        transaction: Option<&OptimisticTransactionDBTransaction>,
        subtrees: &Subtrees,
    ) -> Result<(Vec<Self>, u16), Error> {
        let path_slices = path_query
            .path
            .iter()
            .map(|x| x.as_slice())
            .collect::<Vec<_>>();
        Self::get_query_apply_function(
            merk_path,
            &path_query.query,
            Some(path_slices.as_slice()),
            transaction,
            subtrees,
            Self::path_query_push,
        )
    }

    // Returns a vector of elements, and the number of skipped elements
    pub fn get_sized_query(
        merk_path: &[&[u8]],
        sized_query: &SizedQuery,
        transaction: Option<&OptimisticTransactionDBTransaction>,
        subtrees: &Subtrees,
    ) -> Result<(Vec<Self>, u16), Error> {
        Self::get_query_apply_function(
            merk_path,
            sized_query,
            None,
            transaction,
            subtrees,
            Self::path_query_push,
        )
    }

    /// Insert an element in Merk under a key; path should be resolved and
    /// proper Merk should be loaded by this moment
    /// If transaction is not passed, the batch will be written immediately.
    /// If transaction is passed, the operation will be committed on the
    /// transaction commit.
    pub fn insert<'a: 'b, 'b, K: AsRef<[u8]>>(
        &'a self,
        merk: &mut Merk<PrefixedRocksDbStorage>,
        key: K,
        transaction: Option<&'b <PrefixedRocksDbStorage as Storage>::DBTransaction<'b>>,
    ) -> Result<(), Error> {
        let batch_operations =
            [(
                key,
                Op::Put(bincode::serialize(self).map_err(|_| {
                    Error::CorruptedData(String::from("unable to serialize element"))
                })?),
            )];
        merk.apply::<_, Vec<u8>>(&batch_operations, &[], transaction)
            .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    pub fn iterator(mut raw_iter: RawPrefixedTransactionalIterator) -> ElementsIterator {
        raw_iter.seek_to_first();
        ElementsIterator::new(raw_iter)
    }
}

pub struct ElementsIterator<'a> {
    raw_iter: RawPrefixedTransactionalIterator<'a>,
}

pub fn raw_decode(bytes: &[u8]) -> Result<Element, Error> {
    let tree = <Tree as Store>::decode(bytes).map_err(|e| Error::CorruptedData(e.to_string()))?;
    let element: Element = bincode::deserialize(tree.value())
        .map_err(|_| Error::CorruptedData(String::from("unable to deserialize element")))?;
    Ok(element)
}

impl<'a> ElementsIterator<'a> {
    pub const fn new(raw_iter: RawPrefixedTransactionalIterator<'a>) -> Self {
        ElementsIterator { raw_iter }
    }

    pub fn next(&mut self) -> Result<Option<(Vec<u8>, Element)>, Error> {
        Ok(if self.raw_iter.valid() {
            if let Some((key, value)) = self.raw_iter.key().zip(self.raw_iter.value()) {
                let element = raw_decode(value)?;
                let key_vec = key.to_vec();
                self.raw_iter.next();
                Some((key_vec, element))
            } else {
                None
            }
        } else {
            None
        })
    }
}

#[cfg(test)]
mod tests {
    use merk::test_utils::TempMerk;

    use super::*;
    use crate::tests::{make_grovedb, TEST_LEAF};

    #[test]
    fn test_success_insert() {
        let mut merk = TempMerk::new();
        Element::empty_tree()
            .insert(&mut merk, b"mykey", None)
            .expect("expected successful insertion");
        Element::Item(b"value".to_vec())
            .insert(&mut merk, b"another-key", None)
            .expect("expected successful insertion 2");

        assert_eq!(
            Element::get(&merk, b"another-key").expect("expected successful get"),
            Element::Item(b"value".to_vec()),
        );
    }

    #[test]
    fn test_get_query() {
        let db = make_grovedb();
        let subtrees = db.get_subtrees();

        subtrees
            .borrow_mut([TEST_LEAF], None)
            .expect("cannot borrow a subtree")
            .apply(|merk| {
                Element::Item(b"ayyd".to_vec())
                    .insert(merk, b"d", None)
                    .expect("expected successful insertion");
                Element::Item(b"ayyc".to_vec())
                    .insert(merk, b"c", None)
                    .expect("expected successful insertion");
                Element::Item(b"ayya".to_vec())
                    .insert(merk, b"a", None)
                    .expect("expected successful insertion");
                Element::Item(b"ayyb".to_vec())
                    .insert(merk, b"b", None)
                    .expect("expected successful insertion");
            });

        // Test queries by key
        let mut query = Query::new();
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());
        assert_eq!(
            Element::get_query(&[TEST_LEAF], &query, None, &subtrees)
                .expect("expected successful get_query"),
            vec![
                Element::Item(b"ayya".to_vec()),
                Element::Item(b"ayyc".to_vec())
            ]
        );

        // Test range query
        let mut query = Query::new();
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());
        assert_eq!(
            Element::get_query(&[TEST_LEAF], &query, None, &subtrees)
                .expect("expected successful get_query"),
            vec![
                Element::Item(b"ayya".to_vec()),
                Element::Item(b"ayyb".to_vec()),
                Element::Item(b"ayyc".to_vec())
            ]
        );

        // Test range inclusive query
        let mut query = Query::new();
        query.insert_range_inclusive(b"b".to_vec()..=b"d".to_vec());
        query.insert_range(b"b".to_vec()..b"c".to_vec());
        assert_eq!(
            Element::get_query(&[TEST_LEAF], &query, None, &subtrees)
                .expect("expected successful get_query"),
            vec![
                Element::Item(b"ayyb".to_vec()),
                Element::Item(b"ayyc".to_vec()),
                Element::Item(b"ayyd".to_vec())
            ]
        );

        // Test overlaps
        let mut query = Query::new();
        query.insert_key(b"a".to_vec());
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());
        assert_eq!(
            Element::get_query(&[TEST_LEAF], &query, None, &subtrees)
                .expect("expected successful get_query"),
            vec![
                Element::Item(b"ayya".to_vec()),
                Element::Item(b"ayyb".to_vec()),
                Element::Item(b"ayyc".to_vec())
            ]
        );
    }

    #[test]
    fn test_get_range_query() {
        let db = make_grovedb();
        let subtrees = db.get_subtrees();

        subtrees
            .borrow_mut([TEST_LEAF], None)
            .expect("cannot borrow a subtree")
            .apply(|merk| {
                Element::Item(b"ayyd".to_vec())
                    .insert(merk, b"d", None)
                    .expect("expected successful insertion");
                Element::Item(b"ayyc".to_vec())
                    .insert(merk, b"c", None)
                    .expect("expected successful insertion");
                Element::Item(b"ayya".to_vec())
                    .insert(merk, b"a", None)
                    .expect("expected successful insertion");
                Element::Item(b"ayyb".to_vec())
                    .insert(merk, b"b", None)
                    .expect("expected successful insertion");
            });

        // Test range inclusive query
        let mut query = Query::new();
        query.insert_range(b"a".to_vec()..b"d".to_vec());

        let ascending_query = SizedQuery::new(query.clone(), None, None);
        let (elements, skipped) =
            Element::get_sized_query(&[TEST_LEAF], &ascending_query, None, &subtrees)
                .expect("expected successful get_query");
        assert_eq!(
            elements,
            vec![
                Element::Item(b"ayya".to_vec()),
                Element::Item(b"ayyb".to_vec()),
                Element::Item(b"ayyc".to_vec()),
            ]
        );
        assert_eq!(skipped, 0);

        query.left_to_right = false;

        let backwards_query = SizedQuery::new(query.clone(), None, None);
        let (elements, skipped) =
            Element::get_sized_query(&[TEST_LEAF], &backwards_query, None, &subtrees)
                .expect("expected successful get_query");
        assert_eq!(
            elements,
            vec![
                Element::Item(b"ayyc".to_vec()),
                Element::Item(b"ayyb".to_vec()),
                Element::Item(b"ayya".to_vec()),
            ]
        );
        assert_eq!(skipped, 0);
    }

    #[test]
    fn test_get_range_inclusive_query() {
        let db = make_grovedb();
        let subtrees = db.get_subtrees();

        subtrees
            .borrow_mut([TEST_LEAF], None)
            .expect("cannot borrow a subtree")
            .apply(|merk| {
                Element::Item(b"ayyd".to_vec())
                    .insert(merk, b"d", None)
                    .expect("expected successful insertion");
                Element::Item(b"ayyc".to_vec())
                    .insert(merk, b"c", None)
                    .expect("expected successful insertion");
                Element::Item(b"ayya".to_vec())
                    .insert(merk, b"a", None)
                    .expect("expected successful insertion");
                Element::Item(b"ayyb".to_vec())
                    .insert(merk, b"b", None)
                    .expect("expected successful insertion");
            });

        // Test range inclusive query
        let mut query = Query::new_with_direction(true);
        query.insert_range_inclusive(b"a".to_vec()..=b"d".to_vec());

        let ascending_query = SizedQuery::new(query.clone(), None, None);
        fn check_elements_no_skipped((elements, skipped): (Vec<Element>, u16), reverse: bool) {
            let mut expected = vec![
                Element::Item(b"ayya".to_vec()),
                Element::Item(b"ayyb".to_vec()),
                Element::Item(b"ayyc".to_vec()),
                Element::Item(b"ayyd".to_vec()),
            ];
            if reverse {
                expected.reverse();
            }
            assert_eq!(elements, expected);
            assert_eq!(skipped, 0);
        }

        check_elements_no_skipped(
            Element::get_sized_query(&[TEST_LEAF], &ascending_query, None, &subtrees)
                .expect("expected successful get_query"),
            false,
        );

        query.left_to_right = false;

        let backwards_query = SizedQuery::new(query.clone(), None, None);
        check_elements_no_skipped(
            Element::get_sized_query(&[TEST_LEAF], &backwards_query, None, &subtrees)
                .expect("expected successful get_query"),
            true,
        );

        // Test range inclusive query
        let mut query = Query::new_with_direction(false);
        query.insert_range_inclusive(b"b".to_vec()..=b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());

        let backwards_query = SizedQuery::new(query.clone(), None, None);
        check_elements_no_skipped(
            Element::get_sized_query(&[TEST_LEAF], &backwards_query, None, &subtrees)
                .expect("expected successful get_query"),
            true,
        );
    }

    #[test]
    fn test_get_limit_query() {
        let db = make_grovedb();
        let subtrees = db.get_subtrees();

        subtrees
            .borrow_mut([TEST_LEAF], None)
            .expect("cannot borrow a submerk")
            .apply(|merk| {
                Element::Item(b"ayyd".to_vec())
                    .insert(merk, b"d", None)
                    .expect("expected successful insertion");
                Element::Item(b"ayyc".to_vec())
                    .insert(merk, b"c", None)
                    .expect("expected successful insertion");
                Element::Item(b"ayya".to_vec())
                    .insert(merk, b"a", None)
                    .expect("expected successful insertion");
                Element::Item(b"ayyb".to_vec())
                    .insert(merk, b"b", None)
                    .expect("expected successful insertion");
            });

        // Test queries by key
        let mut query = Query::new_with_direction(true);
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());

        // since these are just keys a backwards query will keep same order
        let backwards_query = SizedQuery::new(query.clone(), None, None);
        let (elements, skipped) =
            Element::get_sized_query(&[TEST_LEAF], &backwards_query, None, &subtrees)
                .expect("expected successful get_query");
        assert_eq!(
            elements,
            vec![
                Element::Item(b"ayya".to_vec()),
                Element::Item(b"ayyc".to_vec()),
            ]
        );
        assert_eq!(skipped, 0);

        // Test queries by key
        let mut query = Query::new_with_direction(false);
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());

        // since these are just keys a backwards query will keep same order
        let backwards_query = SizedQuery::new(query.clone(), None, None);
        let (elements, skipped) =
            Element::get_sized_query(&[TEST_LEAF], &backwards_query, None, &subtrees)
                .expect("expected successful get_query");
        assert_eq!(
            elements,
            vec![
                Element::Item(b"ayyc".to_vec()),
                Element::Item(b"ayya".to_vec()),
            ]
        );
        assert_eq!(skipped, 0);

        // The limit will mean we will only get back 1 item
        let limit_query = SizedQuery::new(query.clone(), Some(1), None);
        let (elements, skipped) =
            Element::get_sized_query(&[TEST_LEAF], &limit_query, None, &subtrees)
                .expect("expected successful get_query");
        assert_eq!(elements, vec![Element::Item(b"ayyc".to_vec()),]);
        assert_eq!(skipped, 0);

        // Test range query
        let mut query = Query::new_with_direction(true);
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());
        let limit_query = SizedQuery::new(query.clone(), Some(2), None);
        let (elements, skipped) =
            Element::get_sized_query(&[TEST_LEAF], &limit_query, None, &subtrees)
                .expect("expected successful get_query");
        assert_eq!(
            elements,
            vec![
                Element::Item(b"ayya".to_vec()),
                Element::Item(b"ayyb".to_vec())
            ]
        );
        assert_eq!(skipped, 0);

        let limit_offset_query = SizedQuery::new(query.clone(), Some(2), Some(1));
        let (elements, skipped) =
            Element::get_sized_query(&[TEST_LEAF], &limit_offset_query, None, &subtrees)
                .expect("expected successful get_query");
        assert_eq!(
            elements,
            vec![
                Element::Item(b"ayyb".to_vec()),
                Element::Item(b"ayyc".to_vec())
            ]
        );
        assert_eq!(skipped, 1);

        // Test range query
        let mut query = Query::new_with_direction(false);
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());

        let limit_offset_backwards_query = SizedQuery::new(query.clone(), Some(2), Some(1));
        let (elements, skipped) =
            Element::get_sized_query(&[TEST_LEAF], &limit_offset_backwards_query, None, &subtrees)
                .expect("expected successful get_query");
        assert_eq!(
            elements,
            vec![
                Element::Item(b"ayyb".to_vec()),
                Element::Item(b"ayya".to_vec())
            ]
        );
        assert_eq!(skipped, 1);

        // Test range inclusive query
        let mut query = Query::new_with_direction(true);
        query.insert_range_inclusive(b"b".to_vec()..=b"d".to_vec());
        query.insert_range(b"b".to_vec()..b"c".to_vec());
        let limit_full_query = SizedQuery::new(query.clone(), Some(5), Some(0));
        let (elements, skipped) =
            Element::get_sized_query(&[TEST_LEAF], &limit_full_query, None, &subtrees)
                .expect("expected successful get_query");
        assert_eq!(
            elements,
            vec![
                Element::Item(b"ayyb".to_vec()),
                Element::Item(b"ayyc".to_vec()),
                Element::Item(b"ayyd".to_vec()),
            ]
        );
        assert_eq!(skipped, 0);

        let mut query = Query::new_with_direction(false);
        query.insert_range_inclusive(b"b".to_vec()..=b"d".to_vec());
        query.insert_range(b"b".to_vec()..b"c".to_vec());

        let limit_offset_backwards_query = SizedQuery::new(query.clone(), Some(2), Some(1));
        let (elements, skipped) =
            Element::get_sized_query(&[TEST_LEAF], &limit_offset_backwards_query, None, &subtrees)
                .expect("expected successful get_query");
        assert_eq!(
            elements,
            vec![
                Element::Item(b"ayyc".to_vec()),
                Element::Item(b"ayyb".to_vec()),
            ]
        );
        assert_eq!(skipped, 1);

        // Test overlaps
        let mut query = Query::new_with_direction(false);
        query.insert_key(b"a".to_vec());
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"b".to_vec()..b"c".to_vec());
        let limit_backwards_query = SizedQuery::new(query.clone(), Some(2), Some(1));
        let (elements, skipped) =
            Element::get_sized_query(&[TEST_LEAF], &limit_backwards_query, None, &subtrees)
                .expect("expected successful get_query");
        assert_eq!(
            elements,
            vec![
                Element::Item(b"ayyb".to_vec()),
                Element::Item(b"ayya".to_vec()),
            ]
        );
        assert_eq!(skipped, 1);
    }
}
