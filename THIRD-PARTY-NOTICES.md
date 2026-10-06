# Third-Party Notices

Fauna is dual-licensed under Apache-2.0 OR MIT (see `LICENSE-APACHE` /
`LICENSE-MIT` and the README's License section). This document lists
third-party material shipped or referenced in this repository that is **not**
covered by that dual license: vendored source, vendored binaries and fixture
assets, and the open-source dependency graphs pulled in by the Go module and
the web (npm) package managers.

Where a licence could not be established from an in-repo LICENSE file,
manifest, or lockfile alone, it is marked `unverified` rather than guessed.
If you can help confirm one of those, please open an issue — see
`CONTRIBUTING.md`.

## Vendored source

Code copied into this repository (in full or as a fork), rather than pulled
in as a package-manager dependency:

| Path | Origin | Licence | Notes |
|---|---|---|---|
| `libs/uniffi-bindgen-cs` | Mozilla's `uniffi-bindgen-cs` C# code generator | MPL-2.0 (`libs/uniffi-bindgen-cs/LICENSE`) | Kept at its upstream licence, per the README's License section. |
| `libs/ksni` | The `ksni` Rust crate (KDE/freedesktop StatusNotifierItem bindings), forked | Unlicense (`libs/ksni/UNLICENSE`) | Kept at its upstream licence. The fork's rationale and the exact diff against the pristine upstream crate are recorded in `libs/ksni/PATCH.md` and `supply-chain/vendored-forks/manifest.json`. |
| `bins/fauna-bridges/third_party/go-imap` | `github.com/emersion/go-imap/v2`, forked | MIT (`bins/fauna-bridges/third_party/go-imap/LICENSE`) | Minimal additive server-framework seams (CONDSTORE/QRESYNC wire emission) on top of the upstream beta; see `bins/fauna-bridges/third_party/go-imap/FORK.md`. Wired via a `replace` directive in `bins/fauna-bridges/go.mod`. |

## Vendored binaries and assets

Binary or generated-elsewhere files committed to the repository:

| Path | Origin | Licence | Notes |
|---|---|---|---|
| `apps/fauna-web/static/c2pa.wasm` | `@contentauth/c2pa-wasm` 0.13.1 (npm), pulled in **transitively** via `@contentauth/c2pa-web` 0.15.2 (`apps/fauna-web/package.json`, `apps/fauna-web/deno.lock`) — the Content Authenticity Initiative's C2PA WebAssembly build | MIT | Read client-side to verify C2PA content-provenance manifests on images (`apps/fauna-web/src/lib/c2pa.ts`). The top-level npm dependency is `@contentauth/c2pa-web`, listed in the web dependency table below; this WASM binary itself ships from the transitive `c2pa-wasm` package, not the top-level one, so the two are recorded separately. Licence per the npm registry's `license` metadata field for `@contentauth/c2pa-wasm@0.13.1` — MIT, by the method `## Corrections` records for 0.4.6. |
| `apps/fauna-android/gradle/wrapper/gradle-wrapper.jar` | The Gradle Wrapper, part of the Gradle build tool | Apache-2.0 | Standard Gradle Wrapper bootstrap jar; the wrapper's target Gradle distribution is pinned in `apps/fauna-android/gradle/wrapper/gradle-wrapper.properties`. |
| `supply-chain/vendored-forks/ksni-0.2.2.crate` | The pristine, unmodified `ksni` 0.2.2 crate as published to crates.io | Unlicense | Kept alongside the fork (`libs/ksni`, above) as the reference copy the re-vendor drift check (`dep-vendor-drift`) compares against; see `docs/goal/architecture/release-integrity.md` § Dependency verification → vendored forks. Not itself linked into any build. |

## External fixtures and test corpora

Test data of external origin, whether committed or fetched at build/test time:

| Path / source | Origin | Licence | Notes |
|---|---|---|---|
| `libs/fauna-segment-store/tests/fixtures/carv2/sample.car` | Origin under review | unverified | A CARv2 (Content-Addressable aRchive) fixture used to test the segment store's CAR reader; provenance is being established separately. |
| ipld/codec-fixtures (github.com/ipld/codec-fixtures) — not a path in this repository | Fetched at test time by `scripts/fetch-dagcbor-fixtures.sh`, at a pinned commit named in `bins/fauna-bridges/internal/dagcbor/CORPUS_COMMIT` | unverified | **Not vendored** — cloned into a scratch directory outside the repository tree when the DAG-CBOR canonicality tests run; no bytes from this corpus are committed here or shipped in the published tree. Listed for transparency about what the test suite pulls in over the network. |

No vendored font files or icon packs of external origin were found under any
app's asset directories in this audit pass (checked by filename convention,
e.g. `*.ttf`/`*.otf`/`*.woff*` and common icon-font/library names — not by
opening binary files). The Windows shell-extension's status icons
(`apps/fauna-windows/shell-ext/src/icons/*.ico`) are original to this project.

## AT Protocol lexicons (`bins/fauna-bridges/internal/atprotolex/lexicons`)

Third-party content vendored wholesale: the AT Protocol Lexicon schema
catalog, copied verbatim from `bluesky-social/atproto`'s `lexicons/`
directory and embedded into the mail/calendar bridge binary at build time.
The PDS write path validates records against these schemas at runtime, so
the tree is committed rather than fetched at boot, refreshed by a dedicated
dev-fleet sync tool; the pinned commit is recorded in
`bins/fauna-bridges/internal/atprotolex/LEXICONS_COMMIT`.

| Path | Origin | Licence | Notes |
|---|---|---|---|
| `bins/fauna-bridges/internal/atprotolex/lexicons` | `bluesky-social/atproto`, `lexicons/` subtree, at the commit pinned in `LEXICONS_COMMIT` | unverified | 396 JSON schema files as of this audit; only `*.json` files are fetched, so no upstream LICENSE file is vendored alongside them. |

## Go modules (`bins/fauna-bridges`)

Every module path recorded in `bins/fauna-bridges/go.sum` — the full
dependency graph (direct and transitive) resolved for the Go mail/calendar
bridge binary. Licences below come from [deps.dev](https://deps.dev)'s GO
package API, consuming only its structured `licenses` verdict field for the
exact resolved version pinned in `go.sum` — never a session reading a
module's own LICENSE file (see `## Corrections` below for the methodology
note). A module deps.dev cannot classify stays `unverified` with the
scanner's own reason attached, rather than a guess. Exact pinned versions
live in `bins/fauna-bridges/go.mod` and `go.sum`, not duplicated here (they
change over time; this table tracks presence and licence, not version).

| Module path | Licence |
|---|---|
| `cloud.google.com/go` | Apache-2.0 OR BSD-3-Clause |
| `cloud.google.com/go/bigquery` | Apache-2.0 |
| `cloud.google.com/go/datastore` | Apache-2.0 |
| `cloud.google.com/go/firestore` | Apache-2.0 |
| `cloud.google.com/go/pubsub` | Apache-2.0 |
| `cloud.google.com/go/storage` | Apache-2.0 |
| `dmitri.shuralyov.com/gpu/mtl` | BSD-3-Clause |
| `github.com/BurntSushi/toml` | MIT |
| `github.com/BurntSushi/xgb` | BSD-3-Clause OR WTFPL OR non-standard |
| `github.com/Jorropo/jsync` | Apache-2.0 OR MIT |
| `github.com/RussellLuo/slidingwindow` | MIT |
| `github.com/antihax/optional` | MIT |
| `github.com/armon/circbuf` | MIT |
| `github.com/armon/go-metrics` | MIT |
| `github.com/armon/go-radix` | MIT |
| `github.com/benbjohnson/clock` | MIT |
| `github.com/beorn7/perks` | MIT |
| `github.com/bgentry/speakeasy` | MIT |
| `github.com/bketelsen/crypt` | MIT |
| `github.com/bluesky-social/indigo` | Apache-2.0 OR MIT |
| `github.com/census-instrumentation/opencensus-proto` | Apache-2.0 |
| `github.com/cespare/xxhash/v2` | MIT |
| `github.com/chzyer/logex` | unverified — deps.dev: no licence data for github.com/chzyer/logex@v1.1.10 |
| `github.com/chzyer/readline` | MIT |
| `github.com/chzyer/test` | MIT |
| `github.com/client9/misspell` | MIT |
| `github.com/cncf/udpa/go` | Apache-2.0 |
| `github.com/coreos/go-semver` | Apache-2.0 |
| `github.com/coreos/go-systemd/v22` | Apache-2.0 |
| `github.com/cpuguy83/go-md2man/v2` | MIT |
| `github.com/cskr/pubsub` | BSD-2-Clause |
| `github.com/davecgh/go-spew` | ISC |
| `github.com/davidlazar/go-crypto` | MIT |
| `github.com/decred/dcrd/dcrec/secp256k1/v4` | ISC |
| `github.com/dustin/go-humanize` | MIT |
| `github.com/earthboundkid/versioninfo/v2` | MIT |
| `github.com/emersion/go-ical` | MIT |
| `github.com/emersion/go-message` | MIT |
| `github.com/emersion/go-sasl` | MIT |
| `github.com/emersion/go-smtp` | MIT |
| `github.com/emersion/go-vcard` | MIT |
| `github.com/emersion/go-webdav` | MIT |
| `github.com/envoyproxy/go-control-plane` | Apache-2.0 |
| `github.com/envoyproxy/protoc-gen-validate` | Apache-2.0 |
| `github.com/fatih/color` | MIT |
| `github.com/felixge/httpsnoop` | MIT |
| `github.com/filecoin-project/go-clock` | MIT |
| `github.com/flynn/noise` | BSD-3-Clause |
| `github.com/francoispqt/gojay` | MIT |
| `github.com/frankban/quicktest` | MIT |
| `github.com/fsnotify/fsnotify` | BSD-3-Clause |
| `github.com/fxamacker/cbor/v2` | MIT |
| `github.com/ghodss/yaml` | BSD-3-Clause OR MIT |
| `github.com/go-gl/glfw` | BSD-3-Clause OR Zlib |
| `github.com/go-gl/glfw/v3.3/glfw` | BSD-3-Clause OR Zlib |
| `github.com/go-logr/logr` | Apache-2.0 |
| `github.com/go-logr/stdr` | Apache-2.0 |
| `github.com/go-redis/redis` | BSD-2-Clause |
| `github.com/go-yaml/yaml` | Apache-2.0 |
| `github.com/godbus/dbus/v5` | BSD-2-Clause |
| `github.com/gogo/protobuf` | BSD-3-Clause |
| `github.com/golang/glog` | Apache-2.0 |
| `github.com/golang/groupcache` | Apache-2.0 |
| `github.com/golang/mock` | Apache-2.0 |
| `github.com/golang/protobuf` | BSD-3-Clause |
| `github.com/google/btree` | Apache-2.0 |
| `github.com/google/go-cmp` | BSD-3-Clause |
| `github.com/google/gofuzz` | Apache-2.0 |
| `github.com/google/gopacket` | BSD-3-Clause |
| `github.com/google/martian` | Apache-2.0 |
| `github.com/google/martian/v3` | Apache-2.0 |
| `github.com/google/pprof` | Apache-2.0 OR BSD-3-Clause |
| `github.com/google/renameio` | Apache-2.0 |
| `github.com/google/uuid` | BSD-3-Clause |
| `github.com/googleapis/gax-go/v2` | BSD-3-Clause |
| `github.com/gopherjs/gopherjs` | BSD-2-Clause |
| `github.com/gorilla/websocket` | BSD-3-Clause |
| `github.com/grpc-ecosystem/grpc-gateway` | Apache-2.0 OR BSD-3-Clause |
| `github.com/hashicorp/consul/api` | MPL-2.0 |
| `github.com/hashicorp/consul/sdk` | MPL-2.0 |
| `github.com/hashicorp/errwrap` | MPL-2.0 |
| `github.com/hashicorp/go-cleanhttp` | MPL-2.0 |
| `github.com/hashicorp/go-hclog` | MIT |
| `github.com/hashicorp/go-immutable-radix` | MPL-2.0 |
| `github.com/hashicorp/go-msgpack` | BSD-3-Clause |
| `github.com/hashicorp/go-multierror` | MPL-2.0 |
| `github.com/hashicorp/go-retryablehttp` | MPL-2.0 |
| `github.com/hashicorp/go-rootcerts` | MPL-2.0 |
| `github.com/hashicorp/go-sockaddr` | MPL-2.0 |
| `github.com/hashicorp/go-syslog` | MIT |
| `github.com/hashicorp/go-uuid` | MPL-2.0 |
| `github.com/hashicorp/go.net` | BSD-3-Clause |
| `github.com/hashicorp/golang-lru` | MPL-2.0 |
| `github.com/hashicorp/golang-lru/v2` | MPL-2.0 |
| `github.com/hashicorp/hcl` | MPL-2.0 |
| `github.com/hashicorp/logutils` | MPL-2.0 |
| `github.com/hashicorp/mdns` | MIT |
| `github.com/hashicorp/memberlist` | MPL-2.0 |
| `github.com/hashicorp/serf` | MPL-2.0 |
| `github.com/huin/goupnp` | BSD-2-Clause |
| `github.com/ianlancetaylor/demangle` | BSD-3-Clause |
| `github.com/inconshreveable/mousetrap` | Apache-2.0 |
| `github.com/ipfs/bbloom` | unverified — deps.dev: no licence data for github.com/ipfs/bbloom@v0.0.4 |
| `github.com/ipfs/boxo` | Apache-2.0 OR MIT |
| `github.com/ipfs/go-bitfield` | Apache-2.0 OR MIT |
| `github.com/ipfs/go-bitswap` | MIT |
| `github.com/ipfs/go-block-format` | MIT |
| `github.com/ipfs/go-blockservice` | MIT |
| `github.com/ipfs/go-cid` | MIT |
| `github.com/ipfs/go-datastore` | MIT |
| `github.com/ipfs/go-detect-race` | MIT |
| `github.com/ipfs/go-ipfs-blockstore` | MIT |
| `github.com/ipfs/go-ipfs-blocksutil` | MIT |
| `github.com/ipfs/go-ipfs-delay` | MIT |
| `github.com/ipfs/go-ipfs-ds-help` | MIT |
| `github.com/ipfs/go-ipfs-exchange-interface` | MIT |
| `github.com/ipfs/go-ipfs-exchange-offline` | MIT |
| `github.com/ipfs/go-ipfs-pq` | MIT |
| `github.com/ipfs/go-ipfs-routing` | MIT |
| `github.com/ipfs/go-ipfs-util` | MIT |
| `github.com/ipfs/go-ipld-cbor` | MIT |
| `github.com/ipfs/go-ipld-format` | MIT |
| `github.com/ipfs/go-ipld-legacy` | Apache-2.0 OR MIT |
| `github.com/ipfs/go-log` | MIT |
| `github.com/ipfs/go-log/v2` | MIT |
| `github.com/ipfs/go-merkledag` | MIT |
| `github.com/ipfs/go-metrics-interface` | MIT |
| `github.com/ipfs/go-peertaskqueue` | Apache-2.0 OR MIT |
| `github.com/ipfs/go-unixfsnode` | Apache-2.0 OR MIT |
| `github.com/ipfs/go-verifcid` | Apache-2.0 OR MIT |
| `github.com/ipld/go-car` | Apache-2.0 OR MIT |
| `github.com/ipld/go-car/v2` | Apache-2.0 OR MIT |
| `github.com/ipld/go-codec-dagpb` | Apache-2.0 OR MIT |
| `github.com/ipld/go-ipld-prime` | MIT |
| `github.com/ipld/go-ipld-prime/storage/bsadapter` | MIT |
| `github.com/jackpal/go-nat-pmp` | Apache-2.0 |
| `github.com/jbenet/go-temp-err-catcher` | MIT |
| `github.com/jbenet/goprocess` | MIT |
| `github.com/jinzhu/inflection` | MIT |
| `github.com/jinzhu/now` | MIT |
| `github.com/json-iterator/go` | MIT |
| `github.com/jstemmer/go-junit-report` | MIT |
| `github.com/jtolds/gls` | MIT |
| `github.com/kisielk/errcheck` | MIT |
| `github.com/kisielk/gotool` | MIT |
| `github.com/klauspost/compress` | BSD-3-Clause |
| `github.com/klauspost/cpuid/v2` | BSD-3-Clause |
| `github.com/koron/go-ssdp` | MIT |
| `github.com/kr/fs` | BSD-3-Clause |
| `github.com/kr/pretty` | MIT |
| `github.com/kr/pty` | MIT |
| `github.com/kr/text` | MIT |
| `github.com/kylelemons/godebug` | Apache-2.0 |
| `github.com/libp2p/go-buffer-pool` | MIT |
| `github.com/libp2p/go-libp2p` | Apache-2.0 OR MIT |
| `github.com/libp2p/go-libp2p-asn-util` | MIT |
| `github.com/libp2p/go-libp2p-record` | MIT |
| `github.com/libp2p/go-libp2p-routing-helpers` | MIT |
| `github.com/libp2p/go-libp2p-testing` | unverified — deps.dev: no licence data for github.com/libp2p/go-libp2p-testing@v0.12.0 |
| `github.com/libp2p/go-msgio` | MIT |
| `github.com/libp2p/go-netroute` | BSD-3-Clause |
| `github.com/magiconair/properties` | BSD-2-Clause |
| `github.com/mattn/go-colorable` | MIT |
| `github.com/mattn/go-isatty` | MIT |
| `github.com/miekg/dns` | BSD-3-Clause |
| `github.com/minio/sha256-simd` | Apache-2.0 |
| `github.com/mitchellh/cli` | MPL-2.0 |
| `github.com/mitchellh/go-homedir` | MIT |
| `github.com/mitchellh/go-testing-interface` | MIT |
| `github.com/mitchellh/gox` | MPL-2.0 |
| `github.com/mitchellh/iochan` | MIT |
| `github.com/mitchellh/mapstructure` | MIT |
| `github.com/modern-go/concurrent` | Apache-2.0 |
| `github.com/modern-go/reflect2` | Apache-2.0 |
| `github.com/mr-tron/base58` | MIT |
| `github.com/multiformats/go-base32` | BSD-3-Clause |
| `github.com/multiformats/go-base36` | unverified — deps.dev: no licence data for github.com/multiformats/go-base36@v0.2.0 |
| `github.com/multiformats/go-multiaddr` | MIT |
| `github.com/multiformats/go-multiaddr-fmt` | MIT |
| `github.com/multiformats/go-multibase` | MIT |
| `github.com/multiformats/go-multicodec` | Apache-2.0 OR MIT |
| `github.com/multiformats/go-multihash` | MIT |
| `github.com/multiformats/go-multistream` | MIT |
| `github.com/multiformats/go-varint` | MIT |
| `github.com/munnerz/goautoneg` | BSD-3-Clause |
| `github.com/ncruces/go-strftime` | MIT |
| `github.com/neelance/astrewrite` | BSD-2-Clause |
| `github.com/neelance/sourcemap` | BSD-2-Clause |
| `github.com/opentracing/opentracing-go` | Apache-2.0 |
| `github.com/pascaldekloe/goe` | unverified — deps.dev: no licence data for github.com/pascaldekloe/goe@v0.0.0-20180627143212-57f6aae5913c |
| `github.com/pelletier/go-toml` | Apache-2.0 OR MIT |
| `github.com/petar/GoLLRB` | BSD-3-Clause |
| `github.com/pion/datachannel` | MIT |
| `github.com/pion/dtls/v2` | MIT |
| `github.com/pion/dtls/v3` | MIT |
| `github.com/pion/ice/v4` | MIT |
| `github.com/pion/interceptor` | MIT |
| `github.com/pion/logging` | MIT |
| `github.com/pion/mdns/v2` | MIT |
| `github.com/pion/randutil` | MIT |
| `github.com/pion/rtcp` | MIT |
| `github.com/pion/rtp` | MIT |
| `github.com/pion/sctp` | MIT |
| `github.com/pion/sdp/v3` | MIT |
| `github.com/pion/srtp/v3` | MIT |
| `github.com/pion/stun` | MIT |
| `github.com/pion/stun/v3` | MIT |
| `github.com/pion/transport/v2` | MIT |
| `github.com/pion/transport/v3` | MIT |
| `github.com/pion/turn/v4` | MIT |
| `github.com/pion/webrtc/v4` | BSD-3-Clause OR MIT |
| `github.com/pkg/errors` | BSD-2-Clause |
| `github.com/pkg/sftp` | BSD-2-Clause |
| `github.com/pmezard/go-difflib` | BSD-3-Clause |
| `github.com/polydawn/refmt` | MIT |
| `github.com/posener/complete` | MIT |
| `github.com/prometheus/client_golang` | Apache-2.0 |
| `github.com/prometheus/client_model` | Apache-2.0 |
| `github.com/prometheus/common` | Apache-2.0 |
| `github.com/prometheus/procfs` | Apache-2.0 |
| `github.com/quic-go/qpack` | MIT |
| `github.com/quic-go/quic-go` | MIT |
| `github.com/quic-go/webtransport-go` | MIT |
| `github.com/remyoudompheng/bigfft` | BSD-3-Clause |
| `github.com/rivo/uniseg` | MIT |
| `github.com/rogpeppe/fastuuid` | BSD-3-Clause |
| `github.com/rogpeppe/go-internal` | BSD-3-Clause |
| `github.com/russross/blackfriday/v2` | BSD-2-Clause |
| `github.com/ryanuber/columnize` | MIT |
| `github.com/sean-/seed` | BSD-3-Clause OR MIT |
| `github.com/shurcooL/go` | BSD-3-Clause OR MIT |
| `github.com/shurcooL/httpfs` | MIT |
| `github.com/shurcooL/sanitized_anchor_name` | MIT |
| `github.com/shurcooL/vfsgen` | MIT |
| `github.com/sirupsen/logrus` | MIT |
| `github.com/smarty/assertions` | Apache-2.0 OR BSD-3-Clause OR MIT |
| `github.com/smartystreets/assertions` | Apache-2.0 OR BSD-3-Clause OR MIT |
| `github.com/smartystreets/goconvey` | Apache-2.0 OR MIT |
| `github.com/spaolacci/murmur3` | BSD-3-Clause |
| `github.com/spf13/afero` | Apache-2.0 |
| `github.com/spf13/cast` | MIT |
| `github.com/spf13/cobra` | Apache-2.0 |
| `github.com/spf13/jwalterweatherman` | MIT |
| `github.com/spf13/pflag` | BSD-3-Clause |
| `github.com/spf13/viper` | MIT |
| `github.com/stretchr/objx` | MIT |
| `github.com/stretchr/testify` | MIT |
| `github.com/subosito/gotenv` | MIT |
| `github.com/teambition/rrule-go` | MIT |
| `github.com/urfave/cli` | MIT |
| `github.com/warpfork/go-testmark` | Apache-2.0 OR MIT |
| `github.com/warpfork/go-wish` | BSD-3-Clause OR MIT |
| `github.com/whyrusleeping/cbor` | Apache-2.0 |
| `github.com/whyrusleeping/cbor-gen` | MIT |
| `github.com/whyrusleeping/chunker` | BSD-2-Clause |
| `github.com/wlynxg/anet` | BSD-3-Clause |
| `github.com/x448/float16` | MIT |
| `github.com/yuin/goldmark` | MIT |
| `gitlab.com/yawning/secp256k1-voi` | Apache-2.0 OR BSD-3-Clause OR MIT |
| `gitlab.com/yawning/tuplehash` | BSD-3-Clause |
| `go.etcd.io/etcd/api/v3` | Apache-2.0 |
| `go.etcd.io/etcd/client/pkg/v3` | Apache-2.0 |
| `go.etcd.io/etcd/client/v2` | Apache-2.0 |
| `go.opencensus.io` | Apache-2.0 |
| `go.opentelemetry.io/auto/sdk` | Apache-2.0 |
| `go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp` | Apache-2.0 |
| `go.opentelemetry.io/otel` | Apache-2.0 |
| `go.opentelemetry.io/otel/metric` | Apache-2.0 |
| `go.opentelemetry.io/otel/sdk` | Apache-2.0 |
| `go.opentelemetry.io/otel/sdk/metric` | Apache-2.0 |
| `go.opentelemetry.io/otel/trace` | Apache-2.0 |
| `go.uber.org/atomic` | MIT |
| `go.uber.org/goleak` | MIT |
| `go.uber.org/mock` | MIT |
| `go.uber.org/multierr` | MIT |
| `go.uber.org/tools` | MIT |
| `go.uber.org/zap` | MIT |
| `go.yaml.in/yaml/v2` | Apache-2.0 |
| `golang.org/x/crypto` | BSD-3-Clause |
| `golang.org/x/exp` | BSD-3-Clause |
| `golang.org/x/image` | BSD-3-Clause |
| `golang.org/x/lint` | BSD-3-Clause |
| `golang.org/x/mobile` | BSD-3-Clause |
| `golang.org/x/mod` | BSD-3-Clause |
| `golang.org/x/net` | BSD-3-Clause |
| `golang.org/x/oauth2` | BSD-3-Clause |
| `golang.org/x/sync` | BSD-3-Clause |
| `golang.org/x/sys` | BSD-3-Clause |
| `golang.org/x/term` | BSD-3-Clause |
| `golang.org/x/text` | BSD-3-Clause |
| `golang.org/x/time` | BSD-3-Clause |
| `golang.org/x/tools` | BSD-3-Clause |
| `golang.org/x/xerrors` | BSD-3-Clause |
| `google.golang.org/api` | BSD-3-Clause |
| `google.golang.org/appengine` | Apache-2.0 |
| `google.golang.org/genproto` | Apache-2.0 |
| `google.golang.org/grpc` | Apache-2.0 |
| `google.golang.org/protobuf` | BSD-3-Clause |
| `gopkg.in/check.v1` | BSD-2-Clause |
| `gopkg.in/errgo.v2` | BSD-3-Clause |
| `gopkg.in/ini.v1` | Apache-2.0 |
| `gopkg.in/yaml.v2` | Apache-2.0 |
| `gopkg.in/yaml.v3` | Apache-2.0 OR MIT |
| `gorm.io/gorm` | MIT |
| `honnef.co/go/tools` | BSD-3-Clause OR MIT |
| `lukechampine.com/blake3` | MIT |
| `modernc.org/cc/v4` | BSD-3-Clause |
| `modernc.org/ccgo/v4` | BSD-3-Clause |
| `modernc.org/fileutil` | BSD-3-Clause |
| `modernc.org/gc/v2` | BSD-3-Clause |
| `modernc.org/gc/v3` | BSD-3-Clause |
| `modernc.org/goabi0` | BSD-3-Clause |
| `modernc.org/libc` | BSD-3-Clause |
| `modernc.org/mathutil` | BSD-3-Clause |
| `modernc.org/memory` | BSD-3-Clause |
| `modernc.org/opt` | BSD-3-Clause |
| `modernc.org/sortutil` | BSD-3-Clause |
| `modernc.org/sqlite` | BSD-3-Clause |
| `modernc.org/strutil` | BSD-3-Clause |
| `modernc.org/token` | BSD-3-Clause |
| `nhooyr.io/websocket` | ISC |
| `rsc.io/binaryregexp` | BSD-3-Clause |
| `rsc.io/quote/v3` | BSD-3-Clause |
| `rsc.io/sampler` | BSD-3-Clause |

324 modules total: 319 with a known licence, 5 marked `unverified`
(deps.dev has no licence data for these; see the entries above).

## Web (npm) dependencies (`apps/fauna-web`)

The top-level dependencies declared in `apps/fauna-web/package.json`
(resolved and pinned in `apps/fauna-web/deno.lock`). "Runtime" ships in the
built SPA bundle; "build-time only" (`devDependencies`) does not.

| Package | Version (spec) | Licence | Kind |
|---|---|---|---|
| `@codemirror/commands` | `6.10.3` | MIT | runtime |
| `@codemirror/state` | `6.6.0` | MIT | runtime |
| `@codemirror/view` | `6.43.1` | MIT | runtime |
| `@contentauth/c2pa-web` | `0.15.2` | unverified | runtime |
| `@sveltejs/adapter-static` | `^3.0.0` | MIT | build-time only |
| `@sveltejs/kit` | `^2.0.0` | MIT | build-time only |
| `@sveltejs/vite-plugin-svelte` | `^6.0.0` | MIT | build-time only |
| `svelte` | `^5.0.0` | MIT | build-time only |
| `svelte-check` | `^4.0.0` | MIT | build-time only |
| `typescript` | `^5.0.0` | Apache-2.0 | build-time only |
| `vite` | `^6.0.0` | MIT | build-time only |

11 top-level packages total: 10 with a known licence, 1 marked `unverified`.

Transitive npm dependencies (resolved in full in `deno.lock`) are not
individually enumerated here; they inherit the same "run a licence scanner
before relying on this document for compliance" caveat as the Go module
table above.

## Swift packages (`apps/fauna-apple`)

Every SwiftPM dependency pinned in `apps/fauna-apple/Package.resolved`
(shared by the macOS and iOS targets).

| Package | Location | Version / Revision | Licence |
|---|---|---|---|
| `sparkle` | `https://github.com/sparkle-project/Sparkle` | 2.9.0 (`21d8df80`) | unverified |

1 top-level package total: 0 with a known licence, 1 marked `unverified`.

## Rust crates

Rust dependency licensing is covered separately by `cargo vet` and the
source/license policy in `deny.toml` (both gated in CI), not by this
document.

## Corrections

This document was generated by a mechanical, best-effort audit of in-repo
manifests, lockfiles, and LICENSE files — it deliberately never reads
third-party source to fill in a licence. If you spot an error, an omission,
or can confirm one of the entries marked `unverified`, please open an issue
(see `CONTRIBUTING.md`).

**2026-08-29 — licence-unknowns drain.** The Go module tables then in this document (`bins/fauna-bridges`,
`tools/atproto-s0-probe` — the probe and its table were removed on 2026-10-05) were re-scanned via [deps.dev](https://deps.dev)'s GO package API
(`GET /v3/systems/GO/packages/{module}/versions/{version}`), reading only its structured
`licenses` verdict field for the exact version pinned in each module's `go.sum` — never the
module's own LICENSE file. 395 of the 398 combined table rows now carry a deps.dev-established
SPDX id (or an SPDX `OR` expression when deps.dev reports more than one licence); the 3 that
remain `unverified` (`github.com/ipfs/bbloom`, `github.com/libp2p/go-libp2p-testing`,
`github.com/multiformats/go-base36`) got a "no licence data" verdict from deps.dev itself, not a
guess. The C2PA wasm binary (`apps/fauna-web/static/c2pa.wasm`) was resolved via the npm
registry's `license` metadata field for `@contentauth/c2pa-wasm@0.4.6`
(`registry.npmjs.org/@contentauth%2Fc2pa-wasm/0.4.6`) — MIT. The vendored `ksni` 0.2.2 crate
(`supply-chain/vendored-forks/ksni-0.2.2.crate`) was confirmed via crates.io's API
(`crates.io/api/v1/crates/ksni/0.2.2`) — Unlicense, matching the value this document already
carried for it. The internal review allowlist's matching `licence: UNKNOWN` entries were
updated the same way.

**2026-10-03 — spam-classifier fixtures removed.** The trained spam-classifier model and
vocabulary fixtures, and the training tool that produced them from the Apache SpamAssassin
public corpus, were removed from the repository together with the classifier that read them.
Their three rows — this document's only `no OSI licence (usage terms only)` entries — are
removed with them.
