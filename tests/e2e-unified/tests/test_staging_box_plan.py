"""tier_1 proofs for ``helpers/staging_box.py`` — the decisions the kept
staging-box live test (``tests/live/test_staging_box_provision.py``) makes
before it spends anything: what belongs to the box, when a run provisions,
resumes or refuses, and how the admin credentials persist between runs.
"""
from __future__ import annotations

import json
import stat
import sys

import pytest

from helpers import staging_box as sb

pytestmark = pytest.mark.tier_1

DOMAIN = "dev.example.test"


def test_server_name_matches_the_orchestrators_scheme():
    assert sb.server_name_for(DOMAIN) == "dev-example-test"


def test_zone_is_the_longest_containing_zone():
    zones = ["example.test", "test", "other.test", "dev.example.test."]
    assert sb.zone_for(DOMAIN, zones) == "dev.example.test"
    assert sb.zone_for(DOMAIN, ["example.test", "other.test"]) == "example.test"
    assert sb.zone_for(DOMAIN, ["other.test", "ample.test"]) is None


def test_dev_and_test_boxes_sit_in_the_uat_zone():
    zones = ["fauna.build"]
    assert sb.zone_for("dev.fauna.build", zones) == "fauna.build"
    assert sb.zone_for("test.fauna.build", zones) == "fauna.build"
    assert sb.zone_for("fauna.build", zones) == "fauna.build"
    assert sb.relative_name("dev.fauna.build", "fauna.build") == "dev"


def test_a_missing_uat_zone_names_the_nameserver_step():
    msg = sb.no_zone_message("dev.fauna.build", ["fauna.social"])
    assert "fauna.build" in msg and "nameservers" in msg and "uat.token" in msg
    other = sb.no_zone_message(DOMAIN, ["other.test"])
    assert "wrong token/project" in other and "nameservers" not in other


def test_relative_name():
    assert sb.relative_name(DOMAIN, "example.test") == "dev"
    assert sb.relative_name("example.test", "example.test.") == "@"


def test_rrsets_under_takes_the_name_and_everything_below_it_only():
    rrsets = [
        {"name": "dev", "type": "A"},
        {"name": "mail.dev", "type": "A"},
        {"name": "s1._domainkey.dev", "type": "TXT"},
        {"name": "@", "type": "A"},
        {"name": "notdev", "type": "A"},
        {"name": "dev2", "type": "A"},
        {"name": "mail", "type": "A"},
    ]
    assert [r["name"] for r in sb.rrsets_under(rrsets, "dev")] == [
        "dev", "mail.dev", "s1._domainkey.dev",
    ]


def test_nothing_at_the_name_provisions():
    assert sb.decide_start(state={"domain": DOMAIN}, servers=[], rrsets=[], rel="dev") \
        == sb.PROVISION


def test_records_elsewhere_in_the_zone_do_not_count():
    zone = [{"name": "@", "type": "A"}, {"name": "mail", "type": "A"}, {"name": "dev2", "type": "A"}]
    assert sb.decide_start(state={"domain": DOMAIN}, servers=[], rrsets=zone, rel="dev") \
        == sb.PROVISION


def test_the_recorded_server_resumes():
    state = {"domain": DOMAIN, "server_id": 7}
    servers = [{"id": 7, "name": "dev-example-test"}]
    assert sb.decide_start(state=state, servers=servers, rrsets=[{"name": "dev"}], rel="dev") \
        == sb.RESUME


@pytest.mark.parametrize(
    "state, servers, rrsets",
    [
        # a server nobody recorded: someone else's, or built but never claimed
        ({"domain": DOMAIN}, [{"id": 7, "name": "dev-example-test"}], []),
        # a server, but not the recorded one
        ({"domain": DOMAIN, "server_id": 3}, [{"id": 7, "name": "dev-example-test"}], []),
        # stray records with no server
        ({"domain": DOMAIN}, [], [{"name": "dev", "type": "A"}]),
        # a recorded server that is gone, its records left behind
        ({"domain": DOMAIN, "server_id": 7}, [], [{"name": "dev", "type": "A"}]),
    ],
)
def test_anything_else_is_refused(state, servers, rrsets):
    with pytest.raises(sb.StagingPlanError):
        sb.decide_start(state=state, servers=servers, rrsets=rrsets, rel="dev")


def test_state_is_minted_once_private_and_reused(tmp_path, monkeypatch):
    monkeypatch.setattr(sb, "STATE_DIR", str(tmp_path / "staging-box"))
    first = sb.load_or_create_state(DOMAIN)
    assert first["handle"] == f"admin@{DOMAIN}"
    assert len(bytes.fromhex(first["secret_hex"])) == 32
    path = sb.state_path(DOMAIN)
    # POSIX mode bits only exist off Windows (os.stat reports 0o666 for any
    # writable file there); privacy there is the per-user profile ACL.
    posix_modes = sys.platform != "win32"
    if posix_modes:
        assert stat.S_IMODE(path.stat().st_mode) == 0o600
        assert stat.S_IMODE(path.parent.stat().st_mode) == 0o700

    first["server_id"] = 7
    sb.save_state(first)
    again = sb.load_or_create_state(DOMAIN)
    assert again == first, "a rerun must reuse the identity and what the run recorded"
    if posix_modes:
        assert stat.S_IMODE(path.stat().st_mode) == 0o600


def test_a_state_file_for_another_box_is_never_overwritten(tmp_path, monkeypatch):
    monkeypatch.setattr(sb, "STATE_DIR", str(tmp_path / "staging-box"))
    path = sb.state_path(DOMAIN)
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps({"domain": "other.example.test", "secret_hex": "00" * 32}))
    with pytest.raises(sb.StagingPlanError):
        sb.load_or_create_state(DOMAIN)
    assert json.loads(path.read_text())["domain"] == "other.example.test"


def test_a_failure_before_the_claim_removes_what_the_run_created():
    servers = [{"id": 7, "name": "dev-example-test"}]
    rrsets = [{"name": "dev", "type": "A"}, {"name": "mail.dev", "type": "A"}]
    plan = sb.cleanup_plan(state={"domain": DOMAIN}, servers=servers,
                           rrsets=rrsets + [{"name": "mail", "type": "A"}], rel="dev")
    assert plan == {"servers": servers, "rrsets": rrsets}


def test_a_failure_after_the_claim_removes_nothing():
    """A recorded box is claimed: a rerun resumes it, so a later step failing
    (mail, certificate, DNS, the serving gate) must never take the box down."""
    state = {"domain": DOMAIN, "server_id": 7}
    plan = sb.cleanup_plan(
        state=state,
        servers=[{"id": 7, "name": "dev-example-test"}],
        rrsets=[{"name": "dev", "type": "A"}],
        rel="dev",
    )
    assert plan == {"servers": [], "rrsets": []}


# ── A box at a zone apex (the box is the apex of its own zone) ────────────
# The zone itself holds NS and SOA at the box's name, and a person's zone may
# hold records the box never published, at the apex and below it. The box owns
# exactly what the nest's record matrix publishes (fixtures/box-dns-names.json).

APEX = "fauna.example"


def _rs(name, rtype, *values):
    return {"name": name, "type": rtype, "records": [{"value": v} for v in values]}


#: What a nest publishes at its apex, as the provider lists it.
APEX_BOX = [
    _rs("@", "A", "192.0.2.10"),
    _rs("@", "MX", "10 mail.fauna.example."),
    _rs("mail", "A", "192.0.2.10"),
    _rs("_dmarc", "TXT", '"v=DMARC1; p=quarantine"'),
    _rs("default._domainkey", "TXT", '"v=DKIM1; k=ed25519; p=abc"'),
    _rs("_mta-sts", "TXT", '"v=STSv1; id=1"'),
    _rs("_smtp._tls", "TXT", '"v=TLSRPTv1; rua=mailto:tlsrpt@fauna.example"'),
    _rs("_caldavs._tcp", "SRV", "0 1 443 mail.fauna.example."),
    _rs("_carddavs._tcp", "SRV", "0 1 443 mail.fauna.example."),
    _rs("_25._tcp.mail", "TLSA", "3 1 1 ab"),
    _rs("_fauna", "TXT", '"self=00"'),
    _rs("_atproto.alice", "TXT", '"did=did:plc:abc"'),
    _rs("relay", "A", "192.0.2.10"),
    _rs("pds", "AAAA", "2001:db8::10"),
    _rs("*", "A", "192.0.2.10"),
]

#: What the zone holds that is not the box's: the zone's own records, a
#: site-verification TXT beside the SPF at the apex, and names the matrix does
#: not publish (including look-alikes of names it does).
APEX_NOT_THE_BOX = [
    _rs("@", "NS", "hydrogen.ns.hetzner.com."),
    _rs("@", "SOA", "hydrogen.ns.hetzner.com. dns.hetzner.com. 1 86400 10800 3600000 3600"),
    _rs("@", "CAA", '0 issue "letsencrypt.org"'),
    _rs("www", "CNAME", "fauna.example."),
    _rs("blog", "A", "198.51.100.7"),
    _rs("x.mail", "A", "198.51.100.7"),
    _rs("_dmarc.blog", "TXT", '"v=DMARC1; p=none"'),
    _rs("a.b._domainkey", "TXT", '"v=DKIM1; p=x"'),
    _rs("mail", "TXT", '"something else"'),
]

APEX_TXT = _rs("@", "TXT", '"google-site-verification=xyz"', '"v=spf1 mx ~all"')


def test_the_apex_is_a_relative_name_of_its_own_zone():
    assert sb.zone_for(APEX, ["example", APEX + "."]) == APEX
    assert sb.relative_name(APEX, APEX) == "@"


def test_an_apex_box_owns_exactly_what_the_matrix_publishes():
    owned = sb.rrsets_under(APEX_BOX + APEX_NOT_THE_BOX + [APEX_TXT], "@")
    spf_only = _rs("@", "TXT", '"v=spf1 mx ~all"')
    assert owned == APEX_BOX + [spf_only]


def test_the_apex_shapes_come_from_the_matrix_fixture():
    shapes = {(s["name"], s["type"]) for s in sb.box_rrset_shapes()}
    assert {("@", "A"), ("@", "MX"), ("@", "TXT"), ("*._domainkey", "TXT")} <= shapes
    assert not {t for _, t in shapes} & {"NS", "SOA", "CAA", "CNAME"}


def test_an_apex_zone_holding_only_its_own_records_provisions():
    """The zone always holds NS/SOA at the apex: that is not the box."""
    zone = APEX_NOT_THE_BOX + [_rs("@", "TXT", '"google-site-verification=xyz"')]
    assert sb.decide_start(state={"domain": APEX}, servers=[], rrsets=zone, rel="@") \
        == sb.PROVISION


@pytest.mark.parametrize("stale", [
    _rs("@", "A", "203.0.113.1"),          # a dead box's apex address
    _rs("@", "TXT", '"v=spf1 mx ~all"'),   # an SPF the box would add a second of
    _rs("s1._domainkey", "TXT", '"v=DKIM1; p=old"'),
])
def test_an_apex_box_record_with_no_recorded_server_is_refused(stale):
    with pytest.raises(sb.StagingPlanError):
        sb.decide_start(state={"domain": APEX}, servers=[],
                        rrsets=APEX_NOT_THE_BOX + [stale], rel="@")


def test_an_apex_cleanup_never_names_ns_soa_or_a_record_not_the_boxs():
    """Production DNS: a failed run at the apex removes the server and the
    box's own records, and of the shared apex TXT the SPF value alone."""
    servers = [{"id": 7, "name": "fauna-example"}]
    plan = sb.cleanup_plan(state={"domain": APEX}, servers=servers,
                           rrsets=APEX_NOT_THE_BOX + APEX_BOX + [APEX_TXT], rel="@")
    assert plan["servers"] == servers
    assert plan["rrsets"] == APEX_BOX + [_rs("@", "TXT", '"v=spf1 mx ~all"')]
    removed = {(r["name"], r["type"], v["value"]) for r in plan["rrsets"] for v in r["records"]}
    for rs in APEX_NOT_THE_BOX:
        for v in rs["records"]:
            assert (rs["name"], rs["type"], v["value"]) not in removed
    assert ("@", "TXT", '"google-site-verification=xyz"') not in removed


def test_an_apex_cleanup_after_the_claim_removes_nothing():
    plan = sb.cleanup_plan(state={"domain": APEX, "server_id": 7},
                           servers=[{"id": 7, "name": "fauna-example"}],
                           rrsets=APEX_BOX, rel="@")
    assert plan == {"servers": [], "rrsets": []}


# ── The update channel — an input, never a constant ─────────────────────────
# One test provisions the organisation's nests, each on its own channel
# (Dev, Test, Stable) (image-pipeline.md § Image tags & channels), so the channel is part of
# the opt-in, and its image tag is read from the one place a channel becomes a
# tag, `UpdateChannel::image_tag`.

def test_the_channel_table_is_read_from_the_rust_mapping():
    assert sb.update_channel_tags() == {"stable": "latest", "test": "test", "dev": "dev"}


@pytest.mark.parametrize("raw,want", [("dev", "dev"), (" Test ", "test"), ("STABLE", "stable")])
def test_a_named_channel_is_taken(raw, want):
    assert sb.staging_channel(raw) == want


@pytest.mark.parametrize("raw", [None, "", "   "])
def test_no_channel_named_is_none(raw):
    assert sb.staging_channel(raw) is None


@pytest.mark.parametrize("raw", ["latest", "prod", "staging"])
def test_an_unknown_channel_is_refused_not_guessed(raw):
    """``latest`` is a TAG, not a channel: the input names the wizard's row."""
    with pytest.raises(sb.StagingPlanError):
        sb.staging_channel(raw)


@pytest.mark.parametrize("channel,tag", [("stable", "latest"), ("test", "test"), ("dev", "dev")])
def test_the_tag_and_the_wizard_row_follow_the_channel(channel, tag):
    assert sb.image_tag_for(channel) == tag
    assert sb.channel_row_id(channel) == f"vps-config-update-channel-row[{channel}]"


@pytest.mark.parametrize("tag", ["latest", "test", "dev"])
def test_the_pull_preflight_reads_the_image_the_cloud_init_pulls(tag):
    """The preflight probes exactly the reference the compose template names —
    read from `cloud_init.rs`, so the two can never disagree."""
    ref = sb.nest_image_ref(tag)
    assert ref == f"ghcr.io/faunasocial/nest:{tag}"
    token_url, manifest_url = sb.anonymous_pull_urls(ref)
    assert token_url == ("https://ghcr.io/token?scope=repository:faunasocial/nest:pull"
                         "&service=ghcr.io")
    assert manifest_url == f"https://ghcr.io/v2/faunasocial/nest/manifests/{tag}"


@pytest.mark.parametrize("ref", ["nest:dev", "faunasocial/nest:dev", "ghcr.io/nest"])
def test_a_reference_without_registry_repository_and_tag_is_refused(ref):
    with pytest.raises(sb.StagingPlanError):
        sb.anonymous_pull_urls(ref)


def _load_live_module(monkeypatch, channel):
    """Import the live test module afresh under a given channel env, so its
    module-level opt-in reads what a run with that env would read."""
    import importlib.util
    from pathlib import Path

    monkeypatch.setenv("FAUNA_E2E_STAGING_BOX", DOMAIN)
    if channel is None:
        monkeypatch.delenv("FAUNA_E2E_STAGING_CHANNEL", raising=False)
    else:
        monkeypatch.setenv("FAUNA_E2E_STAGING_CHANNEL", channel)
    path = Path(__file__).resolve().parent / "live" / "test_staging_box_provision.py"
    spec = importlib.util.spec_from_file_location(f"_staging_live_{channel}", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _channel_skip(mod) -> bool:
    marks = [m for m in mod.pytestmark
             if m.name == "skipif" and "FAUNA_E2E_STAGING_CHANNEL" in m.kwargs.get("reason", "")]
    assert len(marks) == 1, "the live test must gate on the channel exactly once"
    return marks[0].args[0]


def test_the_live_test_skips_without_a_channel(monkeypatch):
    mod = _load_live_module(monkeypatch, None)
    assert _channel_skip(mod) is True
    assert mod.UPDATE_CHANNEL is None


@pytest.mark.parametrize("channel,tag", [("stable", "latest"), ("test", "test"), ("dev", "dev")])
def test_the_live_test_follows_the_named_channel(monkeypatch, channel, tag):
    mod = _load_live_module(monkeypatch, channel)
    assert _channel_skip(mod) is False
    assert (mod.UPDATE_CHANNEL, mod.IMAGE_TAG) == (channel, tag)
