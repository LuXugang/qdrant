use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use atomic_refcell::AtomicRefCell;
use common::cpu::CpuPermit;
use common::types::PointOffsetType;
use rand::Rng;
use sparse::common::sparse_vector::SparseVector;
use sparse::common::sparse_vector_fixture::random_sparse_vector;
use sparse::index::inverted_index::inverted_index_immutable_ram::InvertedIndexImmutableRam;
use sparse::index::inverted_index::InvertedIndex;

use crate::common::operation_error::OperationResult;
use crate::common::rocksdb_wrapper::{open_db, DB_VECTOR_CF};
use crate::fixtures::payload_context_fixture::FixtureIdTracker;
use crate::index::hnsw_index::num_rayon_threads;
use crate::index::sparse_index::sparse_index_config::{SparseIndexConfig, SparseIndexType};
use crate::index::sparse_index::sparse_vector_index::SparseVectorIndex;
use crate::index::struct_payload_index::StructPayloadIndex;
use crate::index::VectorIndex;
use crate::payload_storage::in_memory_payload_storage::InMemoryPayloadStorage;
use crate::vector_storage::simple_sparse_vector_storage::open_simple_sparse_vector_storage;
use crate::vector_storage::VectorStorage;

/// Helper to open a test sparse vector index
pub fn fixture_open_sparse_index<I: InvertedIndex>(
    data_dir: &Path,
    num_vectors: usize,
    full_scan_threshold: usize,
    index_type: SparseIndexType,
    stopped: &AtomicBool,
) -> OperationResult<SparseVectorIndex<I>> {
    // directories
    let index_dir = &data_dir.join("index");
    let payload_dir = &data_dir.join("payload");
    let storage_dir = &data_dir.join("storage");

    // setup
    let id_tracker = Arc::new(AtomicRefCell::new(FixtureIdTracker::new(num_vectors)));
    let payload_storage = InMemoryPayloadStorage::default();
    let wrapped_payload_storage = Arc::new(AtomicRefCell::new(payload_storage.into()));
    let payload_index = StructPayloadIndex::open(
        wrapped_payload_storage,
        id_tracker.clone(),
        payload_dir,
        true,
    )?;
    let wrapped_payload_index = Arc::new(AtomicRefCell::new(payload_index));

    let db = open_db(storage_dir, &[DB_VECTOR_CF]).unwrap();
    let vector_storage = Arc::new(AtomicRefCell::new(open_simple_sparse_vector_storage(
        db,
        DB_VECTOR_CF,
        stopped,
    )?));
    let mut borrowed_storage = vector_storage.borrow_mut();

    // add empty points to storage
    for idx in 0..num_vectors {
        let vec = &SparseVector::new(vec![], vec![]).unwrap();
        borrowed_storage
            .insert_vector(idx as PointOffsetType, vec.into())
            .unwrap();
    }
    drop(borrowed_storage);

    // assert all empty points are in storage
    assert_eq!(
        vector_storage.borrow().available_vector_count(),
        num_vectors,
    );

    let sparse_index_config = SparseIndexConfig::new(Some(full_scan_threshold), index_type);
    let sparse_vector_index: SparseVectorIndex<I> = SparseVectorIndex::open(
        sparse_index_config,
        id_tracker,
        vector_storage.clone(),
        wrapped_payload_index,
        index_dir,
        stopped,
    )?;

    Ok(sparse_vector_index)
}

/// Prepares a sparse vector index with random sparse vectors
pub fn fixture_sparse_index_ram<R: Rng + ?Sized>(
    rnd: &mut R,
    num_vectors: usize,
    max_dim: usize,
    full_scan_threshold: usize,
    data_dir: &Path,
    stopped: &AtomicBool,
) -> SparseVectorIndex<InvertedIndexImmutableRam> {
    fixture_sparse_index_ram_from_iter(
        (0..num_vectors).map(|_| random_sparse_vector(rnd, max_dim)),
        full_scan_threshold,
        data_dir,
        stopped,
        || || (),
    )
}

/// Prepares a sparse vector index with a given iterator of sparse vectors
pub fn fixture_sparse_index_ram_from_iter<P: FnMut()>(
    vectors: impl ExactSizeIterator<Item = SparseVector>,
    full_scan_threshold: usize,
    data_dir: &Path,
    stopped: &AtomicBool,
    progress: impl FnOnce() -> P,
) -> SparseVectorIndex<InvertedIndexImmutableRam> {
    let num_vectors = vectors.len();
    let mut sparse_vector_index = fixture_open_sparse_index(
        data_dir,
        num_vectors,
        full_scan_threshold,
        SparseIndexType::ImmutableRam,
        stopped,
    )
    .unwrap();
    let mut borrowed_storage = sparse_vector_index.vector_storage().borrow_mut();

    // add points to storage
    for (idx, vec) in vectors.enumerate() {
        borrowed_storage
            .insert_vector(idx as PointOffsetType, (&vec).into())
            .unwrap();
    }
    drop(borrowed_storage);

    // assert all points are in storage
    assert_eq!(
        sparse_vector_index
            .vector_storage()
            .borrow()
            .available_vector_count(),
        num_vectors,
    );

    // assert no points are indexed following open for RAM index
    assert_eq!(sparse_vector_index.indexed_vector_count(), 0);

    let permit_cpu_count = num_rayon_threads(0);
    let permit = Arc::new(CpuPermit::dummy(permit_cpu_count as u32));

    // build index to refresh RAM index
    let tick_progress = progress();
    sparse_vector_index
        .build_index_with_progress(permit, stopped, tick_progress)
        .unwrap();
    assert_eq!(sparse_vector_index.indexed_vector_count(), num_vectors);
    sparse_vector_index
}
