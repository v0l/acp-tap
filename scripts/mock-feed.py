#!/usr/bin/env python3
"""Feed acp-tapd with synthetic ACP traffic.

Useful for developing the dashboard without running real agents, and for
producing the screenshot in the README.

    acp-tapd --socket /tmp/acp-tap-mock.sock --listen 127.0.0.1:9112 &
    scripts/mock-feed.py --socket /tmp/acp-tap-mock.sock
"""

import argparse
import json
import socket
import time

PROMPT = """[Base]
You are operating inside an ACP client. Tools are available for reading files,
running commands and editing code. Prefer the smallest change that works.

[System]
You are a code reviewer. Review diffs for over-engineering first: what to
delete, what the standard library already ships, which abstraction has exactly
one caller. Correctness and security in a second pass.

[Agent Memory - core]
Reviewed 41 pull requests this week. The recurring finding is hand-rolled
retry logic around calls that are already idempotent.

Scope: channel
Channel: reviews (#f5894ea9)
Event ID: 78d76018ae1a470347cc05637e62c4d9
From: alejandra
Time: 2026-07-28T09:16:05+00:00
Content: please review PR #280 before it merges - the settlement path changed
Parsed: mentions=[goran]"""


class Feed:
    def __init__(self, path: str, label: str, session: str):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.label = label
        self.session = session

    def send(self, line: dict, direction: str = "to_client") -> None:
        envelope = {
            "label": self.label,
            "dir": direction,
            "ts_ms": int(time.time() * 1000),
            "line": json.dumps(line),
        }
        self.sock.sendall((json.dumps(envelope) + "\n").encode())
        time.sleep(0.01)

    def update(self, update: dict) -> None:
        self.send({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"sessionId": self.session, "update": update},
        })

    def session_new(self) -> None:
        self.send({"jsonrpc": "2.0", "id": 1, "result": {"sessionId": self.session}})

    def prompt(self, text: str) -> None:
        self.send({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/prompt",
            "params": {"sessionId": self.session, "prompt": [{"type": "text", "text": text}]},
        }, direction="to_agent")

    def think(self, text: str) -> None:
        for chunk in text.split(" "):
            self.update({
                "sessionUpdate": "agent_thought_chunk",
                "content": {"type": "text", "text": chunk + " "},
            })

    def say(self, text: str) -> None:
        for chunk in text.split(" "):
            self.update({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": chunk + " "},
            })

    def shell(self, tool_id: str, command: str, output: str, exit_code: int = 0) -> None:
        self.update({
            "sessionUpdate": "tool_call",
            "toolCallId": tool_id,
            "title": "bash",
            "kind": "execute",
            "status": "pending",
            "content": [{"type": "terminal", "terminalId": tool_id}],
        })
        self.update({
            "sessionUpdate": "tool_call_update",
            "toolCallId": tool_id,
            "title": command,
            "status": "in_progress",
        })
        self.update({
            "sessionUpdate": "tool_call_update",
            "toolCallId": tool_id,
            "status": "completed" if exit_code == 0 else "failed",
            "_meta": {
                "terminal_output": {"terminal_id": tool_id, "data": output},
                "terminal_exit": {"terminal_id": tool_id, "exit_code": exit_code},
            },
        })

    def turn_end(self, reason: str = "end_turn") -> None:
        self.send({"jsonrpc": "2.0", "id": 2, "result": {"stopReason": reason}})

    def error(self, message: str) -> None:
        self.send({"jsonrpc": "2.0", "id": 9, "error": {"code": -32601, "message": message}})


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--socket", default="/tmp/acp-tap-mock.sock")
    ap.add_argument(
        "--hold",
        type=float,
        default=0,
        help="keep connections open for N seconds so agents stay 'connected' "
        "and the last turn stays in flight",
    )
    args = ap.parse_args()

    # An idle agent, so the sidebar shows more than one state.
    idle = Feed(args.socket, "codex", "sess-codex-1")
    idle.session_new()
    idle.prompt("check whether the migration is reversible")
    idle.say("It is: 20260115151245_external_ids.sql drops cleanly, no data loss.")
    idle.turn_end()

    # An agent that hit a protocol error.
    broken = Feed(args.socket, "gemini", "sess-gemini-1")
    broken.session_new()
    broken.error('"Method not found": session/set_model')

    # The agent in the foreground, mid-review.
    goran = Feed(args.socket, "goran", "sess-goran-7")
    goran.session_new()
    goran.prompt(PROMPT)
    goran.think(
        "The settlement path changed, so the risk is double-crediting on retry. "
        "Check whether the ledger write is idempotent before reading the diff."
    )
    goran.shell(
        "bash_0",
        "gh pr diff 280 --name-only",
        "crates/api/src/settlement.rs\ncrates/api/src/ledger.rs\ncrates/api/tests/settlement.rs\n",
    )
    goran.shell(
        "bash_1",
        "cargo test -p api settlement",
        "running 7 tests\n"
        "test settlement::credits_once_per_invoice ... ok\n"
        "test settlement::rejects_replayed_preimage ... ok\n"
        "test settlement::partial_failure_rolls_back ... ok\n"
        "test result: ok. 7 passed; 0 failed\n",
    )
    goran.shell(
        "bash_2",
        "rg -n 'retry' crates/api/src/settlement.rs",
        "44:    // retry wrapper around an idempotent write\n52:        retry(3, || ledger.credit(invoice))\n",
    )
    goran.say(
        "settlement.rs:L44-52: delete: retry wrapper around an idempotent ledger write. "
        "The preimage check already makes a replay a no-op, so the retry only widens the "
        "window where two workers race. Nothing replaces it.\n\n"
        "net: -18 lines possible.\n\nShip after: drop the retry."
    )
    goran.turn_end()

    # Leave one agent visibly mid-turn.
    live = Feed(args.socket, "marta", "sess-marta-3")
    live.session_new()
    live.prompt("the invoice page shows prices a hundred times too large")
    live.think("Minor units. The loader is probably passing the raw integer to the formatter.")
    live.shell("bash_9", "rg -n 'smallestUnitScale' src/", "src/utils/currency.ts:12:export function smallestUnitScale(\n")

    time.sleep(1)
    print("mock feed sent")
    if args.hold:
        time.sleep(args.hold)


if __name__ == "__main__":
    main()
