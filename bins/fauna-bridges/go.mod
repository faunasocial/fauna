module github.com/faunasocial/fauna/bins/fauna-bridges

go 1.26.0

toolchain go1.26.8

require (
	github.com/BurntSushi/toml v1.6.0
	github.com/emersion/go-ical v0.0.0-20250609112844-439c63cef608
	github.com/emersion/go-imap/v2 v2.0.0-beta.8
	github.com/emersion/go-sasl v0.0.0-20241020182733-b788ff22d5a6
	github.com/emersion/go-smtp v0.24.0
	// go-vcard is pulled in directly ahead of the CardDAV terminator's use of
	// go-webdav/carddav (which imports it): pre-warmed here so the module +
	// go.sum zip hash are already present. A bare `go mod tidy` would prune
	// this line until an import lands in slice 2c, so keep it explicit.
	github.com/emersion/go-vcard v0.0.0-20230815062825-8fda7d206ec9
	github.com/emersion/go-webdav v0.7.0
	github.com/faunasocial/fauna/libs/fauna-mail-go v0.0.0
	github.com/fxamacker/cbor/v2 v2.9.2
	github.com/ipfs/go-cid v0.6.1
	github.com/ipld/go-car/v2 v2.16.0
	github.com/mr-tron/base58 v1.3.0
	github.com/multiformats/go-multihash v0.2.3
	github.com/prometheus/client_golang v1.23.2
	lukechampine.com/blake3 v1.4.1
	nhooyr.io/websocket v1.8.17
)

require (
	github.com/bluesky-social/indigo v0.0.0-20260529183052-5368f55344e0
	// Direct for TESTS ONLY (internal/atprotofirehose): indigo's own repo-stream
	// consumer, events.HandleRepoStream, takes a gorilla connection, and running
	// our firehose frames through that exact parser is the Sync v1.1 conformance
	// proof. The bridge's own WS code — client and server — is nhooyr; nothing in
	// production imports gorilla. Already in the graph via indigo; this line only
	// promotes it from indirect. A bare `go mod tidy` keeps it (the test imports
	// it) — see also the go-log replace at the bottom, which tidy must not drop.
	github.com/gorilla/websocket v1.5.3
	github.com/ipfs/go-block-format v0.2.3
	github.com/ipfs/go-ipld-format v0.6.3
	golang.org/x/crypto v0.57.0
	modernc.org/sqlite v1.54.0
)

require (
	github.com/RussellLuo/slidingwindow v0.0.0-20200528002341-535bb99d338b // indirect
	github.com/beorn7/perks v1.0.1 // indirect
	github.com/cespare/xxhash/v2 v2.3.0 // indirect
	github.com/dustin/go-humanize v1.0.1 // indirect
	github.com/earthboundkid/versioninfo/v2 v2.24.1 // indirect
	github.com/emersion/go-message v0.18.2 // indirect
	github.com/felixge/httpsnoop v1.0.4 // indirect
	github.com/go-logr/logr v1.4.3 // indirect
	github.com/go-logr/stdr v1.2.2 // indirect
	github.com/gogo/protobuf v1.3.2 // indirect
	github.com/google/uuid v1.6.0 // indirect
	github.com/hashicorp/go-cleanhttp v0.5.2 // indirect
	github.com/hashicorp/go-retryablehttp v0.7.7 // indirect
	github.com/hashicorp/golang-lru v1.0.2 // indirect
	github.com/hashicorp/golang-lru/v2 v2.0.7 // indirect
	github.com/ipfs/bbloom v0.0.4 // indirect
	github.com/ipfs/boxo v0.34.0 // indirect
	github.com/ipfs/go-blockservice v0.5.2 // indirect
	github.com/ipfs/go-datastore v0.8.3 // indirect
	github.com/ipfs/go-ipfs-blockstore v1.3.1 // indirect
	github.com/ipfs/go-ipfs-ds-help v1.1.1 // indirect
	github.com/ipfs/go-ipfs-exchange-interface v0.2.1 // indirect
	github.com/ipfs/go-ipfs-util v0.0.3 // indirect
	github.com/ipfs/go-ipld-cbor v0.2.1 // indirect
	github.com/ipfs/go-ipld-legacy v0.2.2 // indirect
	github.com/ipfs/go-log v1.0.5 // indirect
	github.com/ipfs/go-log/v2 v2.8.1 // indirect
	github.com/ipfs/go-merkledag v0.11.0 // indirect
	github.com/ipfs/go-metrics-interface v0.3.0 // indirect
	github.com/ipfs/go-verifcid v0.0.3 // indirect
	github.com/ipld/go-car v0.6.1-0.20230509095817-92d28eb23ba4 // indirect
	github.com/ipld/go-codec-dagpb v1.7.0 // indirect
	github.com/ipld/go-ipld-prime v0.23.0 // indirect
	github.com/jinzhu/inflection v1.0.0 // indirect
	github.com/jinzhu/now v1.1.5 // indirect
	github.com/klauspost/cpuid/v2 v2.3.0 // indirect
	github.com/kylelemons/godebug v1.1.0 // indirect
	github.com/mattn/go-isatty v0.0.20 // indirect
	github.com/minio/sha256-simd v1.0.1 // indirect
	github.com/multiformats/go-base32 v0.1.0 // indirect
	github.com/multiformats/go-base36 v0.2.0 // indirect
	github.com/multiformats/go-multibase v0.3.0 // indirect
	github.com/multiformats/go-multicodec v0.10.0 // indirect
	github.com/multiformats/go-varint v0.1.0 // indirect
	github.com/munnerz/goautoneg v0.0.0-20191010083416-a7dc8b61c822 // indirect
	github.com/ncruces/go-strftime v1.0.0 // indirect
	github.com/opentracing/opentracing-go v1.2.0 // indirect
	github.com/petar/GoLLRB v0.0.0-20210522233825-ae3b015fd3e9 // indirect
	github.com/polydawn/refmt v0.89.1-0.20231129105047-37766d95467a // indirect
	github.com/prometheus/client_model v0.6.2 // indirect
	github.com/prometheus/common v0.66.1 // indirect
	github.com/prometheus/procfs v0.17.0 // indirect
	github.com/remyoudompheng/bigfft v0.0.0-20230129092748-24d4a6f8daec // indirect
	github.com/rivo/uniseg v0.1.0 // indirect
	github.com/spaolacci/murmur3 v1.1.0 // indirect
	github.com/teambition/rrule-go v1.8.2 // indirect
	github.com/whyrusleeping/cbor v0.0.0-20171005072247-63513f603b11 // indirect
	github.com/whyrusleeping/cbor-gen v0.3.1 // indirect
	github.com/x448/float16 v0.8.4 // indirect
	gitlab.com/yawning/secp256k1-voi v0.0.0-20230925100816-f2616030848b // indirect
	gitlab.com/yawning/tuplehash v0.0.0-20230713102510-df83abbf9a02 // indirect
	go.opentelemetry.io/auto/sdk v1.2.1 // indirect
	go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp v0.62.0 // indirect
	go.opentelemetry.io/otel v1.42.0 // indirect
	go.opentelemetry.io/otel/metric v1.42.0 // indirect
	go.opentelemetry.io/otel/trace v1.42.0 // indirect
	go.uber.org/atomic v1.11.0 // indirect
	go.uber.org/multierr v1.11.0 // indirect
	go.uber.org/zap v1.27.0 // indirect
	go.yaml.in/yaml/v2 v2.4.2 // indirect
	golang.org/x/exp v0.0.0-20250813145105-42675adae3e6 // indirect
	golang.org/x/sys v0.48.0 // indirect
	golang.org/x/time v0.12.0 // indirect
	golang.org/x/xerrors v0.0.0-20240903120638-7835f813f4da // indirect
	google.golang.org/protobuf v1.36.9 // indirect
	gorm.io/gorm v1.25.9 // indirect
	modernc.org/libc v1.74.1 // indirect
	modernc.org/mathutil v1.7.1 // indirect
	modernc.org/memory v1.11.0 // indirect
)

replace github.com/faunasocial/fauna/libs/fauna-mail-go => ../../libs/fauna-mail-go

// FAUNA-FORK: vendored emersion/go-imap/v2 with minimal additive
// server-framework seams (CONDSTORE/QRESYNC wire emission). Upstream
// beta.8 is API-unstable pre-1.0 and lacks these seams; see
// third_party/go-imap/FORK.md (fork-vs-upstream decision tracked internally).
replace github.com/emersion/go-imap/v2 => ./third_party/go-imap

// Diamond-conflict pin (S3 atproto PDS, internal/atprotorepo). indigo's
// atproto/repo pulls the legacy IPFS stack (go-car v1 -> go-merkledag ->
// go-log v1.0.5), whose levels.go needs go-log/v2's LevelFromString; but the
// bridge's modern go-car/v2 + boxo force go-log/v2 up to v2.8.1, which removed
// it. v2.5.1 is the version indigo + merkledag want and still has the symbol,
// and the go-car/v2 read path (internal/dagcbor) builds+tests green against it.
// Do NOT remove without re-checking `go build ./internal/atprotorepo/`.
replace github.com/ipfs/go-log/v2 => github.com/ipfs/go-log/v2 v2.5.1
