"""Test plotting and the real table_demo CLI with a local OpenAI test server."""
import json
import os
from pathlib import Path
import runpy
import shutil
import socket
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock

from PIL import Image
from fae_plugin import HookNext, ToolRequest

ROOT = Path(__file__).resolve().parent
TABLE = runpy.run_path(str(ROOT / "table_image/plugin.py"))
CHART = runpy.run_path(str(ROOT / "table_line_chart/plugin.py"))
ARGUMENTS = {
    "path": "plugin-table.png", "x": ["Jan", "Feb", "Mar"], "y": [120, 180, 150],
    "title": "Sales", "x_label": "Month", "y_label": "Revenue",
}


class PlotTests(unittest.TestCase):
    def test_single_flat_negative_numeric_and_many_points(self):
        cases = [
            (["one"], [0]), (["a", "b"], [5, 5]), ([1, 2, 10], [-3, 0, -1]),
            (list(range(100)), list(range(100))), ([0, 1], [-1e308, 1e308]),
        ]
        with tempfile.TemporaryDirectory() as directory:
            for x, y in cases:
                with self.subTest(x=x, y=y):
                    result = CHART["render_line_chart"](
                        {**ARGUMENTS, "path": "line.png", "x": x, "y": y}, directory)
                    with Image.open(result["path"]) as image:
                        self.assertEqual(image.format, "PNG")
                        self.assertEqual(image.size, (1000, 600))
                        self.assertGreater(sum(count for count, color in image.getcolors(1000000)
                                               if color == (37, 99, 235)), 0)

    def test_invalid_data(self):
        cases = [
            ([], []), (["a"], [1, 2]), (["a"], ["1"]), (["a"], [True]),
            (["a"], [float("inf")]), ([float("nan")], [1]),
            (["a"], [float("nan")]), (["a\nb"], [1]), ([{}], [1]),
        ]
        for x, y in cases:
            with self.subTest(x=x, y=y):
                with self.assertRaises(ValueError):
                    CHART["chart_data"]({**ARGUMENTS, "x": x, "y": y})


class ChainTests(unittest.IsolatedAsyncioTestCase):
    async def test_routes_each_tool_in_either_chain_order(self):
        with tempfile.TemporaryDirectory() as directory:
            session = SimpleNamespace(send=AsyncMock())
            ctx = SimpleNamespace(scope=AsyncMock(return_value=SimpleNamespace(workspace=directory)),
                                  session=session)
            chart, table = CHART["Table2Tool"](), TABLE["TableImageTool"]()
            chart._channel = table._channel = "fae_table_image"
            for outer, inner in ((chart, table), (table, chart)):
                for suffix, field, y_type in (("generate", "rows", ["string", "number"]),
                                              ("line_chart", "points", "number")):
                    with self.subTest(outer=type(outer).__name__, tool=suffix):
                        terminal = AsyncMock(side_effect=AssertionError("No matching tool"))

                        async def describe(name):
                            return await inner.desc(ctx, name, next=terminal)

                        name = f"fae_table_image__{suffix}"
                        description = await outer.desc(
                            ctx, name, next=HookNext(describe, lambda value: None))
                        self.assertEqual(description["name"], name)
                        self.assertEqual(description["parameters"]["properties"]["y"]["items"],
                                         {"type": y_type})

                        async def execute(request):
                            return await inner.exec(ctx, request, next=terminal)

                        arguments = {**ARGUMENTS, "path": f"{suffix}.png"}
                        if suffix == "generate":
                            arguments["y"] = ["low", "high", "medium"]
                        response = await outer.exec(
                            ctx, ToolRequest({"tool_name": name, "arguments": json.dumps(arguments)}),
                            next=HookNext(execute, lambda value: None))
                        result = json.loads(response.result)
                        self.assertEqual(result["path"], str(Path(directory) / f"{suffix}.png"))
                        self.assertEqual(result[field], 3)
                        self.assertNotIn("chart_path", result)
                        with Image.open(result["path"]) as image:
                            image.verify()
                        terminal.assert_not_awaited()
            self.assertEqual(session.send.await_count, 4)
            self.assertEqual({path.name for path in Path(directory).iterdir()},
                             {"generate.png", "line_chart.png"})

    async def test_invalid_y_does_not_call_inner_plugin(self):
        tool = CHART["Table2Tool"]()
        tool._channel = "fae_table_image"
        inner = AsyncMock()
        with self.assertRaises(ValueError):
            await tool.exec(SimpleNamespace(session=None,
                                           scope=AsyncMock(return_value=SimpleNamespace(workspace=None))),
                            ToolRequest({"tool_name": "fae_table_image__line_chart",
                                         "arguments": json.dumps({**ARGUMENTS, "y": ["bad"] * 3})}),
                            next=HookNext(inner, lambda value: None))
        inner.assert_not_awaited()


class DemoTests(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("FAE_TEST_BINARY"),
                         "Set FAE_TEST_BINARY to a freshly built fae executable")
    def test_table_demo_selects_line_chart(self):
        self.run_demo("line_chart")

    @unittest.skipUnless(os.environ.get("FAE_TEST_BINARY"),
                         "Set FAE_TEST_BINARY to a freshly built fae executable")
    def test_table_demo_selects_table(self):
        self.run_demo("generate")

    def run_demo(self, suffix):
        requests = []
        name = f"fae_table_image__{suffix}"
        arguments = {**ARGUMENTS, "path": f"{suffix}.png"}
        if suffix == "generate":
            arguments["y"] = ["low", "high", "medium"]

        class ModelServer(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(payload)
                results = [message["content"] for message in payload["messages"]
                           if message["role"] == "tool"]
                if results:
                    delta = {"role": "assistant", "content": results[-1]}
                    reason = "stop"
                else:
                    user = next(message["content"] for message in reversed(payload["messages"])
                                if message["role"] == "user")
                    chosen = "line_chart" if "line chart" in str(user) else "generate"
                    delta = {"role": "assistant", "tool_calls": [{
                        "index": 0, "id": "call_table", "type": "function",
                        "function": {"name": f"fae_table_image__{chosen}",
                                     "arguments": json.dumps(arguments)},
                    }]}
                    reason = "tool_calls"
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                for value, finish in [(delta, None), ({}, reason)]:
                    chunk = {"id": "table-demo-test", "object": "chat.completion.chunk",
                             "created": 0, "model": "gpt-4o-mini",
                             "choices": [{"index": 0, "delta": value, "finish_reason": finish}]}
                    self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()

        with tempfile.TemporaryDirectory(prefix="fae-table-demo-") as directory:
            home = Path(directory) / "home"
            workspace = Path(directory) / "workspace"
            (home / "plugins").mkdir(parents=True)
            (home / "agents").mkdir()
            workspace.mkdir()
            for agent_file in ("table_demo_config.json", "table_demo_prompt.txt"):
                shutil.copy(ROOT / "agents" / agent_file, home / "agents" / agent_file)
            model = ThreadingHTTPServer(("127.0.0.1", 0), ModelServer)
            thread = threading.Thread(target=model.serve_forever, daemon=True)
            thread.start()
            with socket.socket() as port:
                port.bind(("127.0.0.1", 0))
                server_port = port.getsockname()[1]
            env = {**os.environ, "FAE_PLUGIN_PYTHON": os.environ.get("FAE_PLUGIN_PYTHON", os.sys.executable),
                   "OPENAI_API_KEY": "local-test-key",
                   "OPENAI_BASE_URL": f"http://127.0.0.1:{model.server_port}/v1",
                   "NO_PROXY": "127.0.0.1,localhost"}
            command = [str(Path(os.environ["FAE_TEST_BINARY"]).resolve()),
                       "--fae-home", str(home), "--workspace", str(workspace), "--color", "never"]

            def run(*arguments):
                result = subprocess.run([*command, *arguments], env=env, text=True,
                                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=45)
                self.assertEqual(result.returncode, 0, result.stdout)
                return result.stdout

            try:
                for plugin in ("table_line_chart", "table_image"):
                    run("plugin", "install", str(ROOT / plugin))
                run("server", "start", "--port", str(server_port))
                for plugin in ("table_line_chart", "table_image"):
                    status = json.loads(run("plugin", "show", plugin))
                    self.assertTrue(status[plugin]["loaded"], status)
                    self.assertEqual(status[plugin]["chain_order"], ["table_line_chart", "table_image"])
                kind = "line chart" if suffix == "line_chart" else "table"
                output = run("agent", "--agent-id", "table_demo", "--session-id", "table-choice-test",
                             f"Generate only a {kind} from " + json.dumps(arguments))
                marker = "[table2-plugin]" if suffix == "line_chart" else "[table-image-plugin]"
                other = "[table-image-plugin]" if suffix == "line_chart" else "[table2-plugin]"
                self.assertIn(marker, output)
                self.assertNotIn(other, output)
                self.assertIn(f"{suffix}.png", output)
                self.assertGreaterEqual(len(requests), 2)
                tool_results = [message["content"] for message in requests[-1]["messages"]
                                if message["role"] == "tool"]
                self.assertEqual(len(tool_results), 1)
                result = json.loads(tool_results[0])
                self.assertEqual(result["points" if suffix == "line_chart" else "rows"], 3)
                self.assertEqual(Path(result["path"]), workspace / f"{suffix}.png")
                self.assertNotIn("chart_path", result)
                with Image.open(result["path"]) as image:
                    image.verify()
                self.assertEqual({path.name for path in workspace.iterdir()}, {f"{suffix}.png"})
                if suffix == "line_chart":
                    with Image.open(result["path"]) as image:
                        self.assertEqual(image.size, (1000, 600))
                self.assertEqual(requests[0]["model"], "gpt-4o-mini")
                tools = {tool["function"]["name"]: tool["function"] for tool in requests[0]["tools"]}
                self.assertEqual(set(tools),
                                 {"fae_table_image__generate", "fae_table_image__line_chart"})
                self.assertIn("折线图生成", tools["fae_table_image__line_chart"]["description"])
                self.assertIn("表格生成", tools["fae_table_image__generate"]["description"])
                self.assertEqual(tools["fae_table_image__line_chart"]["parameters"]["properties"]["y"]["items"],
                                 {"type": "number"})
                calls = [call["function"]["name"] for message in requests[-1]["messages"]
                         for call in message.get("tool_calls", [])]
                self.assertEqual(calls, [name])
            finally:
                try:
                    run("server", "stop")
                finally:
                    model.shutdown()
                    model.server_close()
                    thread.join(timeout=5)


if __name__ == "__main__":
    unittest.main(verbosity=2)
