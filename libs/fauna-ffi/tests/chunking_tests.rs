use fauna_ffi::*;

#[test]
fn chunk_small_file_returns_single_chunk() {
    let data = vec![42u8; 1024];
    let manifest = chunk_file(data);
    assert_eq!(manifest.file_hash.len(), 32);
    assert_eq!(manifest.total_size, 1024);
    assert_eq!(manifest.chunk_hashes.len(), 1);
    assert_eq!(manifest.chunk_sizes[0], 1024);
}

#[test]
fn extract_and_reassemble_roundtrip() {
    let data = vec![7u8; 2048];
    let manifest = chunk_file(data.clone());
    let chunks = extract_chunks(data.clone(), manifest).unwrap();
    let reassembled = reassemble_chunks(chunks).unwrap();
    assert_eq!(reassembled, data);
}

#[test]
fn content_hash_deterministic() {
    let data = b"hello fauna".to_vec();
    let h1 = ffi_content_hash(data.clone());
    let h2 = ffi_content_hash(data);
    assert_eq!(h1, h2);
    assert_eq!(h1.len(), 32);
}

#[test]
fn chunk_file_at_path_writes_chunks_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test.bin");
    std::fs::write(&file_path, vec![42u8; 1024]).unwrap();

    let result = chunk_file_at_path(file_path.to_str().unwrap().into()).unwrap();
    assert!(result.chunk_dir.is_some());
    let chunk_dir = result.chunk_dir.unwrap();
    let hash_hex = hex::encode(&result.manifest.chunk_hashes[0]);
    let chunk_path = format!("{}/{}.bin", chunk_dir, hash_hex);
    assert!(std::path::Path::new(&chunk_path).exists());
}

#[test]
fn content_hash_at_path_matches_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test.bin");
    let data = b"hello streaming hash".to_vec();
    std::fs::write(&file_path, &data).unwrap();

    let h_mem = ffi_content_hash(data);
    let h_path = content_hash_at_path(file_path.to_str().unwrap().into()).unwrap();
    assert_eq!(h_mem, h_path);
}

#[test]
fn reassemble_chunks_to_path_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let data = vec![7u8; 2048];
    let manifest = chunk_file(data.clone());

    let chunk_dir = dir.path().join("chunks");
    std::fs::create_dir(&chunk_dir).unwrap();
    let hash_hex = hex::encode(&manifest.chunk_hashes[0]);
    std::fs::write(chunk_dir.join(format!("{}.bin", hash_hex)), &data).unwrap();

    let output = dir.path().join("output.bin");
    reassemble_chunks_to_path(
        chunk_dir.to_str().unwrap().into(),
        manifest,
        output.to_str().unwrap().into(),
    )
    .unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), data);
}

#[test]
fn serialize_deserialize_manifest_roundtrip() {
    let data = vec![99u8; 512];
    let manifest = chunk_file(data);
    let manifest_bytes = serialize_manifest(manifest.clone()).unwrap();
    let decoded = deserialize_manifest(manifest_bytes).unwrap();
    assert_eq!(manifest.file_hash, decoded.file_hash);
    assert_eq!(manifest.total_size, decoded.total_size);
}

#[test]
fn generate_device_id_unique() {
    let id1 = generate_device_id();
    let id2 = generate_device_id();
    assert_eq!(id1.len(), 32);
    assert_ne!(id1, id2);
}
