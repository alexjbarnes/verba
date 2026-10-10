# Optional article sync

Draft architecture, 17 September 2026. Investigation only, not implemented.

## Recommendation

Use a private Cloudflare R2 bucket, accessed directly from Verba's Rust backend through its S3-compatible API. Keep local storage authoritative for offline use, with R2 exchanging article content and reading state between devices.

For a personal phone and Mac library, no Worker, hosted database, login service or subscription is needed. Both devices point at the same bucket and library prefix, using separately revocable credentials. A bucket URL alone is not enough: each device also needs an access key and secret.

Cloudflare's SQLite-based service is D1. It would work behind a small sync API, but does not remove the need to design offline changes, conflicts and deletions. Start with R2. Add a Worker and D1 only if this becomes a managed multi-user service or the library needs server-side queries.

Do not upload and overwrite `library.json` wholesale. Two offline devices would overwrite each other's changes, including articles added on the other device.

## What exists today

The current app already shares its Rust backend and vanilla JS reader between Android and desktop.

- `src-tauri/src/library.rs:34`: `LibraryItem` contains article body, title, URL, tags, progress, seen status and provenance. Books use the same model but keep chapter bodies separately.
- `src-tauri/src/library.rs:154`: IDs are hexadecimal microsecond timestamps. There are no sync revisions, pending changes or deletion tombstones.
- `src-tauri/src/library.rs:379`: Android uses its app data directory. Desktop uses `dirs::config_dir()/verba`.
- `src-tauri/src/library.rs:442`: every library mutation rewrites the JSON file directly. Most callers log save failures rather than returning them. Loading failure currently falls back to an empty library.
- `src/main.js:761`: reading position is scroll fraction multiplied by JavaScript string length, not a measured text anchor. Saves are throttled to roughly four seconds, with a forced save on reader close. Resume reverses that calculation.
- `src/main.js:907`: programmatic scrolling and user scrolling share the same save handler. Sync must not turn a restore operation into a new user progress update.
- `src/main.js:1931` and `src/main.js:1960`: feed imports enter through `library_add`. Existing URL deduplication is local to each device.
- `src-tauri/src/feeds.rs:30`: subscriptions, processed entry keys and HTTP validators live separately from the library.
- `src-tauri/src/config.rs:5`: current persisted configuration contains only language. Display preferences also live in frontend localStorage.
- `src-tauri/tauri.conf.json:29`: macOS packaging is configured, with minimum macOS 14.4 and ad-hoc signing. Actual Mac build and installation still need testing on a Mac.

No existing sync or account system is wired into the current backend. Historical authentication plans are not a prerequisite for this feature.

## First version

Sync saved articles, including pasted text, their extracted bodies, title, source URL, publication information, remote image URLs, tags, seen status, reading position and deletion.

Article text remains available offline on both devices. Transfer the extracted body exactly as saved, including heading, link and image markers. Never re-extract the website on the receiving device: changed text would invalidate progress and could reintroduce extraction bugs.

Keep books, local EPUB covers, feed subscriptions, feed polling state and display preferences local for now. Articles imported from feeds still sync. Preserve their provenance as descriptive metadata without creating feed subscriptions on the receiving device. A missing local feed continues to fall back to the article URL's hostname.

Images remain remote URLs, as they are today. This is offline text sync, not a full offline website archive. Caching image bytes and syncing books are separate follow-ups.

## Shape

```text
Android reader                         Mac reader
      |                                     |
Rust library + sync                   Rust library + sync
      |                                     |
Local JSON + durable pending state    Local JSON + durable pending state
      |                                     |
      +-------- HTTPS / signed S3 ----------+
                        |
                 Private R2 bucket
```

Use one small mutable index and immutable article bodies:

```text
verba/<library-id>/v1/index.json
verba/<library-id>/v1/bodies/<sha256>.txt
```

`index.json` contains a schema version, library ID, and records keyed by a globally unique article sync ID. Each live record contains metadata, a body hash, mutable field revisions and reading state. Deleted records remain as small tombstones. Bodies are UTF-8, addressed by SHA-256 of their exact bytes, and uploaded before the index references them.

The body is separate so scrolling does not re-upload article text. The index itself is fetched and conditionally replaced as one object. This is deliberately an O(library size) metadata transfer, suitable for a personal collection rather than a large shared catalogue.

Start with explicit safety limits, for example a 5 MiB index and 10 MiB per article body. Check existing local items before enrolment, validate downloaded bytes before parsing, and show a clear error instead of silently truncating or skipping an oversized library. Split the index only when measured library size makes this necessary.

## Conflict handling

R2 provides storage, not a merge engine. Merge runs in Rust and must give the same result regardless of download order.

Give each installation a random device UUID and a persisted logical counter. A local edit advances that counter. After merging remote records, advance past the largest observed counter. A revision is `(counter, device_id)`, compared lexicographically. The device ID breaks ties deterministically without trusting wall-clock time.

Use separate revisions for tags and reading state, rather than one revision for the entire article. This preserves a tag edit from one device while another changes progress. Treat reading position and its body hash as one value. Treat each complete tag list as one value. Concurrent edits to the same value resolve to the greater revision, not a union of incompatible states.

This ordering preserves edits made after observing another edit. For genuinely concurrent offline edits, it chooses a deterministic winner, not necessarily the last action in real time. That is the deliberate first-version compromise. Do not describe it as exact last-used-device tracking.

Seen status merges with logical OR because the current action only sets it to true. Progress must not merge using the greatest percentage: resetting progress or deliberately reading backwards must work.

A deletion permanently wins over other updates to that article ID. Retain its tombstone indefinitely in v1 so a device returning after months offline cannot resurrect it. Re-adding a deleted article creates a new ID. Clearing tombstones requires a later explicit library reset or device acknowledgement protocol, not a time-based expiry.

Assign a UUID `sync_id` to existing local articles during enrolment and use UUIDs for newly created articles. Keep old local IDs intact where needed for existing UI and file references. Persist the mapping once. Do not derive identity from timestamp IDs or assume a URL uniquely identifies every saved copy.

Independently importing the same URL on two offline devices can create two articles in v1. Downloading the same sync ID never creates a duplicate. Leave duplicate consolidation out of the initial protocol rather than silently discard a differently extracted copy. In the intended phone-first setup, keep each feed's auto-import on one device, then sync its saved articles to the other.

## One sync cycle

1. Capture a local snapshot of pending changes under the library lock, then release it before network work.
2. Upload missing immutable bodies. Use conditional creation (`If-None-Match: *`). An already-existing object is acceptable only after confirming it represents the expected bytes.
3. GET the index and its ETag, or use a conditional GET against the last downloaded ETag. For a new library, initialise with `If-None-Match: *`. Distinguish a missing object from authentication errors and malformed data.
4. Validate its schema, library ID, IDs, revisions, field sizes and body references. Merge remote state with the captured local changes.
5. If changes remain to publish, PUT the merged index with `If-Match: <downloaded ETag>`. On a precondition failure, fetch again, re-merge and retry with bounded backoff. Never fall back to an unconditional overwrite.
6. Download required bodies, verify their hashes and apply complete records locally. Keep an item pending rather than publish a local record with missing content. A failing download does not block already-cached reading.
7. Persist the merged state and acknowledgements atomically. Clear only the exact local revisions included in the successful publication. Edits made during the request remain pending. Notify the frontend to refresh its library view.

An uncertain PUT result is safe to retry: read the index again and merge the same revisions. A crash after publication but before acknowledgement likewise repeats an idempotent operation.

R2 documents both `If-Match` and `If-None-Match` support for S3 `PutObject`, plus strong consistency for direct reads, writes and listings. Use the authenticated S3 endpoint, not a public custom domain with caching. Treat ETags as opaque concurrency tokens, not body hashes.

Before implementation is accepted, prove conditional writes with two real clients against a disposable bucket prefix. The chosen Rust client must transmit the conditions correctly. A successful ordinary upload is not enough to establish safety.

## Local durability comes first

Keep JSON for the first version. A local SQLite migration is not required solely to add sync.

Extend the local library file into a versioned envelope containing items, sync IDs, revisions, tombstones, the logical counter and publication state. These must commit together. A separate pending queue written after `library.json` would leave a crash window where a saved edit never syncs.

Read the old array format and migrate once, keeping a backup. Use temporary-file write, flush and atomic replacement under one mutation lock. Return persistence failures through Tauri instead of acknowledging success after logging an error. Do not replace unreadable state with an empty library and upload it. Show recovery-required status and keep the original bytes.

Do not let the existing reload-on-list path replace state while a sync merge or local edit is committing. Apply mutations, merge results and disk replacement through the same serialised library path. No lock is held across network requests.

A separate non-secret sync configuration stores endpoint, bucket, library ID and device identity. Immutable downloaded bodies can be staged separately. An orphan staged body is recoverable; a committed library record pointing to a missing body is not.

## Reading hand-off

Sync on app launch, foreground/resume, explicit Sync now and shortly after local edits. While active, check periodically, initially every 60 seconds. Debounce progress uploads to roughly 15 seconds rather than sending a request for every scroll. Force a local progress save on reader close and background transition, then attempt network sync without assuming the OS will let it finish.

Android background execution is best effort. If the phone goes offline or is killed before upload, the Mac sees the last successfully synced position. Display last successful sync and pending changes so this is visible.

On the Mac, refresh remote state before opening an article when possible, but do not make offline reading wait indefinitely for the network. Keep the library as the landing screen. Do not auto-open an article simply because it changed remotely.

Do not move an already-open reader when another device updates progress. Keep incoming state available for the next open, and do not save a stale scroll position merely because that reader closes without any user movement. Suppress persistence during initial render, resume and layout-driven scroll changes. Only real reading activity or an explicit reset creates a progress revision.

The current scroll-fraction position is approximate across different screens, font settings and image loading. Preserve it initially and label hand-off as approximate. Exact paragraph continuity needs a stable rendered-text anchor plus an offset, with a defined UTF-16 or Unicode coordinate system. That can follow without changing the storage service. Always bind progress to the exact body hash so it cannot be applied to unrelated content.

## Setup and credentials

Settings needs an optional Sync section with account endpoint, bucket, library ID, access key and secret, plus Test connection, Sync now and Disconnect. Show Disabled, Syncing, Up to date, Offline/pending, or a useful error with the last successful sync time.

Use R2 Standard storage and a dedicated private bucket. The normal endpoint is `https://<account-id>.r2.cloudflarestorage.com`, with S3 region `auto`. Use the endpoint shown by Cloudflare if the bucket has a jurisdiction-specific endpoint.

Create separate Object Read & Write credentials for each device, scoped to this bucket. Normal token scoping is bucket-level. A prefix organises data but is not an access-control boundary. Never reuse credentials for unrelated private files or bundle a developer-owned account credential in the app.

Keep credentials in platform-protected storage: macOS Keychain and Android Keystore-backed encrypted storage. The Android Keystore protects an encryption key, rather than storing an arbitrary secret string directly. Credential integration on both platforms is an implementation prerequisite, not a reason to fall back to plain TOML or frontend localStorage. Secrets are accepted through a dedicated command, never returned by general configuration reads or written to logs.

Perform S3 calls and SigV4 signing in Rust. Reuse the existing `reqwest`, Tokio and serde stack where practical, with a maintained S3/signing dependency rather than handwritten cryptography. Confirm conditional-request support and Android build size before choosing that dependency. No frontend S3 SDK or bucket CORS configuration is needed for Rust HTTP calls.

TLS and provider-side encryption protect transport and stored objects, but this proposal is not end-to-end encryption. Anyone holding the bucket credentials, and the storage provider within its service boundary, can access article content and reading history. Treat remote content as untrusted: validate it and retain the reader's escaping and URL restrictions.

Disconnect removes local credentials and stops sync. It does not delete either copy of the library. Changing bucket or library ID requires explicit join/create confirmation so unrelated libraries are not accidentally merged. Joining an existing library merges local articles with it, never replaces either side wholesale.

Account creation, QR pairing, public sharing and managed authentication are intentionally absent. Add a Worker-backed account layer if users should enable sync without operating their own Cloudflare bucket.

## Costs and limits

Cloudflare's published R2 Standard free tier currently includes 10 GB-month storage, one million Class A operations and ten million Class B operations per month. Beyond that, listed rates are $0.015/GB-month, $4.50/million Class A operations and $0.36/million Class B operations, with no internet egress charge. These are account-level allowances, not a guarantee that enabling sync cannot produce a bill.

For scale, 1,000 articles averaging 100 KB of text occupy about 100 MB before metadata. Personal phone/Mac usage should fit comfortably within the published allowances if uploads are debounced. GET checks still consume operations even when nothing changes. Avoid Infrequent Access storage for frequently changing sync data.

Retain tombstones and orphan remote bodies in v1. This uses extra storage but avoids unsafe garbage collection while devices may be offline. Sync is also not backup: deletions propagate, so retain independent local backups before enrolment and migration.

## Implementation outline

First make library persistence crash-safe and add schema migration, sync IDs, revisions and pending-state tests. Keep all existing local-only use working with sync disabled.

Then add `src-tauri/src/sync.rs` for direct R2 access, merge and retry logic, wire its commands and events through `lib.rs`, and add protected credential storage. Integrate settings and reader lifecycle handling in `src/main.js` and `src/index.html`. This does not need a generic storage-provider framework.

Finish with a two-device trial covering offline adds, a tag edit concurrent with progress, a deliberate progress reset, deletion against an offline edit, stale ETags, an uncertain upload response, interrupted local writes, unavailable bodies, wrong credentials and unsupported schemas. Verify that restoring scroll creates no new edit, backgrounding preserves pending work, and a sleeping reader does not overwrite another device's newer progress.

Run the same trial on Android and a real Mac, including different font sizes and images loading late. Nothing in this document establishes that the Mac package has already been built or that precise cross-screen anchors already work.

## Sources

Cloudflare documentation consulted for this draft:

- [R2 S3 API compatibility, including conditional PutObject](https://developers.cloudflare.com/r2/api/s3/api/)
- [R2 consistency model and cache caveats](https://developers.cloudflare.com/r2/reference/consistency/)
- [R2 credentials and bucket-level permissions](https://developers.cloudflare.com/r2/api/tokens/)
- [R2 S3 client configuration](https://developers.cloudflare.com/r2/get-started/s3/)
- [R2 pricing](https://developers.cloudflare.com/r2/pricing/)
- [D1 overview](https://developers.cloudflare.com/d1/)
