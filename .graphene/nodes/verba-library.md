---
type: module
summary: Rust library store: saved articles in library.json, per-item progress, tags, seen, archived; Tauri library_* commands
entry_points:
  - src-tauri/src/library.rs
  - src-tauri/src/lib.rs
covers:
  - src-tauri/src/library.rs
  - src-tauri/src/lib.rs
last_commit: 12ebb616a103c7f54cb562830c524f729e4d5099
---

- Archive added 2026-09-24: LibraryItem.archived (serde default false, so legacy JSON loads), Library::set_archived, command library_set_archived(id, archived). Archive only flips the flag; progress/tags untouched so unarchive restores exactly. Archived items stay in library_list, so feed dedupe (libraryUrlSet, byGuid 'Added' badges) still sees them and never re-imports. Every mutator follows the same lock, find, mutate, save_to_disk pattern; there is no partial write. <!-- id:fd2d -->
- Books removed 2026-09-24 (book_* commands, add_book/chapter/set_book_position/mark_chapter_completed/set_book_read, EPUB covers set_cover/cover + library_set_cover/library_cover, books_dir/book_path/cover_path, book file IO; base64 dep and tempfile dev-dep dropped). LEGACY DATA KEPT: LibraryItem still has chapters/current_chapter/completed (ChapterMeta kept) so old books round-trip through saves untouched; list() (and so get()) filters out any item with non-empty chapters, so they're hidden, not deleted. books/<id>.json and cover files are left on disk. Don't strip those fields without a migration, or the next save drops old books' metadata and they reappear as empty articles. <!-- id:51cc -->
