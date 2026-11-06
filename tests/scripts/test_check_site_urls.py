"""Unit tests for scripts/check-site-urls.py — the built site's no-trailing-slash
gate (front-door.md § Public-vhost topology), on throwaway trees."""

import importlib.util
from pathlib import Path

_SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "check-site-urls.py"
_spec = importlib.util.spec_from_file_location("check_site_urls", _SCRIPT)
G = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(G)


def test_a_slashed_internal_page_url_is_refused():
    assert G.problem("/privacy/", G.ORIGIN)
    assert G.problem("https://fauna.social/organization/bylaws/", G.ORIGIN)
    assert G.problem("//fauna.social/who/", G.ORIGIN)


def test_the_root_and_slash_less_urls_pass():
    for url in ("/", "https://fauna.social/", "https://fauna.social", "/privacy",
                "/privacy?x=1#top", "/apps/cli/client-metadata.json", "/_astro/a.css"):
        assert G.problem(url, G.ORIGIN) is None, url


def test_other_sites_and_non_page_schemes_pass():
    for url in ("https://github.com/faunasocial/fauna/", "mailto:a@example.org",
                "data:image/png;base64,AA", "#main", "?q=1"):
        assert G.problem(url, G.ORIGIN) is None, url


def test_a_relative_url_is_refused():
    assert G.problem("features", G.ORIGIN)
    assert G.problem("./who", G.ORIGIN)


def test_check_reads_links_canonical_og_url_refresh_and_sitemap(tmp_path):
    (tmp_path / "who").mkdir()
    (tmp_path / "who" / "index.html").write_text(
        '<link rel="canonical" href="https://fauna.social/who/">'
        '<meta property="og:url" content="https://fauna.social/who/">'
        '<meta http-equiv="refresh" content="0;url=/apps/">'
        '<a href="/features/">f</a><a href="/privacy">p</a><a href="/">home</a>',
        encoding="utf-8")
    (tmp_path / "sitemap.xml").write_text(
        "<urlset><url><loc>https://fauna.social/</loc></url>"
        "<url><loc>https://fauna.social/what/</loc></url></urlset>", encoding="utf-8")
    found = G.check(tmp_path)
    assert len(found) == 5, found
    assert any("og:url" in f for f in found)
    assert any("meta refresh" in f for f in found)
    assert any("/features/" in f for f in found)
    assert any(f.startswith("sitemap.xml") and "/what/" in f for f in found)


def test_a_clean_tree_passes_and_main_exits_zero(tmp_path, capsys):
    (tmp_path / "index.html").write_text(
        '<link rel="canonical" href="https://fauna.social/"><a href="/who">w</a>',
        encoding="utf-8")
    assert G.main([str(tmp_path)]) == 0
    (tmp_path / "bad.html").write_text('<a href="/who/">w</a>', encoding="utf-8")
    assert G.main([str(tmp_path)]) == 1
    assert "bad.html" in capsys.readouterr().err


def test_no_built_site_is_an_error_not_a_pass(tmp_path):
    assert G.main([str(tmp_path / "absent")]) == 2
