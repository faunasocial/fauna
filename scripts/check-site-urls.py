#!/usr/bin/env python3
"""Fail on a slashed page URL in the built fauna.social site.

front-door.md § Public-vhost topology: a fauna.social page URL carries no
trailing slash (`/privacy`, not `/privacy/`), the root `/` aside. Both hosts
answer the slashed form with a permanent redirect, so a slashed link still
lands; but every link the site prints is a redirect hop and a second spelling
of the same page, and a slashed canonical or og:url tells search engines and
link previews the redirect is the page. This walks the BUILT output (`just
site`'s `dist/`), so it judges what visitors get, whatever source produced it:
a component, the copy in docs/sites/fauna-social/, the sitemap.

What it reads: every `href`, `src`, `action`, the canonical `<link>`, the
`og:url` meta and the meta-refresh target of each `.html` file, and every
`<loc>` of `sitemap.xml`. What it refuses:

- an internal URL (root-relative, or absolute on the site's own origin) whose
  path ends in `/` and is not the root;
- a relative URL (no scheme, no leading `/`, `#` or `?`). The pages are
  directory pages served at their slash-less URL, so a relative reference
  resolves against the PARENT directory — `x` on `/apps` names `/x`, not
  `/apps/x`.

Usage: check-site-urls.py <dist-dir> [--origin https://fauna.social]
"""
from __future__ import annotations

import argparse
import re
import sys
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urlsplit

ORIGIN = "https://fauna.social"

_SCHEME = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:")
_LOC = re.compile(r"<loc>([^<]*)</loc>")
_REFRESH_URL = re.compile(r"url\s*=\s*['\"]?([^'\";]+)", re.IGNORECASE)


def problem(url: str, origin: str) -> str | None:
    """Why `url` breaks the rule, or None when it keeps it."""
    url = url.strip()
    if not url or url.startswith(("#", "?")):
        return None
    if url.startswith("//"):
        # Scheme-relative: internal only when it names our own host.
        url = urlsplit(origin).scheme + ":" + url
    if _SCHEME.match(url):
        parts = urlsplit(url)
        own = urlsplit(origin)
        if (parts.scheme, parts.netloc) != (own.scheme, own.netloc):
            return None  # another site, or mailto:/data:/…
        path = parts.path or "/"
    elif url.startswith("/"):
        path = urlsplit(url).path
    else:
        return "relative URL (resolves against the parent of a slash-less page)"
    if path != "/" and path.endswith("/"):
        return "trailing slash (the page's URL is " + repr(path.rstrip("/")) + ")"
    return None


class _Collector(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.urls: list[tuple[int, str, str]] = []

    def handle_starttag(self, tag, attrs):
        a = {k: (v or "") for k, v in attrs}
        line = self.getpos()[0]
        for name in ("href", "src", "action"):
            if name in a:
                self.urls.append((line, f"<{tag} {name}>", a[name]))
        if tag == "meta":
            if a.get("property") == "og:url" and "content" in a:
                self.urls.append((line, "og:url", a["content"]))
            if a.get("http-equiv", "").lower() == "refresh":
                m = _REFRESH_URL.search(a.get("content", ""))
                if m:
                    self.urls.append((line, "meta refresh", m.group(1)))

    handle_startendtag = handle_starttag


def html_urls(text: str) -> list[tuple[int, str, str]]:
    c = _Collector()
    c.feed(text)
    c.close()
    return c.urls


def check(dist: Path, origin: str = ORIGIN) -> list[str]:
    offenders = []
    for path in sorted(dist.rglob("*.html")):
        rel = path.relative_to(dist)
        for line, where, url in html_urls(path.read_text(encoding="utf-8")):
            why = problem(url, origin)
            if why:
                offenders.append(f"{rel}:{line}: {where} {url!r}: {why}")
    sitemap = dist / "sitemap.xml"
    if sitemap.is_file():
        for url in _LOC.findall(sitemap.read_text(encoding="utf-8")):
            why = problem(url, origin)
            if why:
                offenders.append(f"sitemap.xml: <loc> {url!r}: {why}")
    return offenders


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("dist", type=Path)
    ap.add_argument("--origin", default=ORIGIN)
    args = ap.parse_args(argv)
    if not args.dist.is_dir() or not any(args.dist.rglob("*.html")):
        print(f"check-site-urls: no built site at {args.dist}", file=sys.stderr)
        return 2
    offenders = check(args.dist, args.origin)
    if offenders:
        print("check-site-urls: page URLs carry no trailing slash and no relative "
              "form (front-door.md § Public-vhost topology):", file=sys.stderr)
        for o in offenders:
            print("  " + o, file=sys.stderr)
        return 1
    print("check-site-urls: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
