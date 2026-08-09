"""Where the session snapshot actually is inside `herdr api snapshot` output.

One function, in one file, because getting this wrong is silent and has now cost
three separate fixes in this repository. `herdr api snapshot` prints the whole
socket response, and `session.snapshot` answers as

    {"id": ..., "result": {"type": "session_snapshot", "snapshot": {...}}}

so the `SessionSnapshot` is two levels below the top. Read the top level and
every key is missing: `workspaces` is absent rather than wrong, so a walk over it
yields nothing, and any comparison built on it comes back equal-and-empty instead
of failing. `src/cli/api.rs` carries the same warning for `api status get`, which
"always misses and reports every session as unset" when read one level too high.

The snapshot itself is four flat arrays — `workspaces`, `tabs`, `panes`,
`agents` — with `tokens` an object on workspaces and panes alike. There is no
`workspaces[].tabs[].panes[]` nesting; a walk expecting one finds nothing.
"""


def snapshot_of(doc):
    """Return the SessionSnapshot from a CLI response, or `doc` if already bare.

    Accepts a bare snapshot so a caller can hand this either form — a file
    written by `api snapshot` today, or a payload lifted straight off the socket.
    """
    if isinstance(doc, dict):
        result = doc.get("result")
        if isinstance(result, dict):
            inner = result.get("snapshot")
            if isinstance(inner, dict):
                return inner
    return doc


def looks_like_snapshot(doc):
    """True when `doc` has the shape a snapshot comparison can read.

    Used to fail loudly at the point of reading rather than to let an unwrap that
    silently returned the wrong level turn into a vacuous pass downstream.
    """
    return isinstance(doc, dict) and all(
        isinstance(doc.get(key), list) for key in ("workspaces", "tabs", "panes")
    )
