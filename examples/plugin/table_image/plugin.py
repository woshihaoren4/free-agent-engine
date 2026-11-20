"""Render paired x/y values into a PNG table with Pillow."""
import asyncio
import json
from pathlib import Path
from typing import Any

from PIL import Image, ImageDraw, ImageFont
from fae_plugin import ChainToolsHook, Context, HookNext, ToolRequest, ToolResponse


def render_table(arguments: dict[str, Any], workspace: str | None) -> dict[str, Any]:
    path = arguments.get("path")
    x, y = arguments.get("x"), arguments.get("y")
    if not isinstance(path, str) or not path.strip():
        raise ValueError("path must be a nonempty PNG path")
    if not isinstance(x, list) or not isinstance(y, list) or not 1 <= len(x) <= 100:
        raise ValueError("x and y must be arrays with 1..100 paired rows")
    if len(x) != len(y):
        raise ValueError("x and y must have equal lengths")
    if any(type(value) not in (str, int, float) for value in x + y):
        raise ValueError("x/y cells must be strings or numbers")

    output = Path(path).expanduser()
    if not output.is_absolute():
        if workspace is None:
            raise ValueError("A relative path requires an Agent workspace; otherwise use an absolute path")
        output = Path(workspace) / output
    output = output.resolve()
    if output.suffix.lower() != ".png":
        raise ValueError("path must end with .png")

    title = arguments.get("title", "X / Y Table")
    x_label, y_label = arguments.get("x_label", "X"), arguments.get("y_label", "Y")
    cells = [[x_label, y_label], *[[str(a), str(b)] for a, b in zip(x, y)]]
    if not isinstance(title, str) or any(not isinstance(c, str) for row in cells for c in row):
        raise ValueError("title and column labels must be strings")
    if any(len(c) > 120 or "\n" in c or "\r" in c for c in [title, *[c for row in cells for c in row]]):
        raise ValueError("Each title/label/cell must be a single line of at most 120 characters")
    font_path = arguments.get("font_path")
    if font_path is not None:
        if not isinstance(font_path, str) or not Path(font_path).is_absolute():
            raise ValueError("font_path must be an absolute TTF/OTF/TTC path")
        font = ImageFont.truetype(font_path, size=20)
    else:
        font = ImageFont.load_default(size=20)

    margin, padding, row_height, title_height = 24, 18, 44, 48
    widths = [
        max(180, int(max(font.getlength(row[column]) for row in cells)) + padding * 2)
        for column in range(2)
    ]
    width = max(sum(widths), int(font.getlength(title)) + padding * 2) + margin * 2
    height = margin * 2 + title_height + row_height * len(cells)
    with Image.new("RGB", (width, height), "#ffffff") as image:
        draw = ImageDraw.Draw(image)
        draw.text((margin, margin), title, font=font, fill="#172554")
        for row_index, row in enumerate(cells):
            top = margin + title_height + row_index * row_height
            left = margin
            for column, value in enumerate(row):
                fill = "#1e40af" if row_index == 0 else ("#eff6ff" if row_index % 2 else "#ffffff")
                draw.rectangle((left, top, left + widths[column], top + row_height),
                               fill=fill, outline="#cbd5e1", width=1)
                draw.text((left + padding, top + row_height / 2), value, font=font,
                          anchor="lm", fill="#ffffff" if row_index == 0 else "#1e293b")
                left += widths[column]
        output.parent.mkdir(parents=True, exist_ok=True)
        image.save(output, format="PNG")
    return {"path": str(output), "rows": len(x), "width": width, "height": height}


class TableImageTool(ChainToolsHook):
    async def desc(self, ctx: Context, tool_name: str, *,
                   next: HookNext[str, dict[str, Any]]) -> dict[str, Any]:
        if tool_name != f"{self.channel()}__generate":
            return await next(tool_name)
        return {
            "name": tool_name,
            "description": "表格生成：将等长 x/y 数据排成两列表格并保存 PNG，展示明细并返回真实文件路径。每对数据是一行。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "输出 .png 路径；相对路径基于 Agent workspace"},
                    "x": {"type": "array", "minItems": 1, "maxItems": 100,
                          "items": {"type": ["string", "number"]}, "description": "第一列"},
                    "y": {"type": "array", "minItems": 1, "maxItems": 100,
                          "items": {"type": ["string", "number"]}, "description": "第二列，长度必须与 x 相同"},
                    "title": {"type": "string", "description": "图片标题，默认 X / Y Table"},
                    "x_label": {"type": "string", "description": "第一列表头，默认 X"},
                    "y_label": {"type": "string", "description": "第二列表头，默认 Y"},
                    "font_path": {"type": "string", "description": "可选绝对字体路径；中文需使用支持中文的字体"},
                },
                "required": ["path", "x", "y"],
                "additionalProperties": False,
            },
        }

    async def exec(self, ctx: Context, request: ToolRequest, *,
                   next: HookNext[ToolRequest, ToolResponse]) -> ToolResponse:
        if request.tool_name != f"{self.channel()}__generate":
            return await next(request)
        arguments = request.arguments_json()
        if not isinstance(arguments, dict):
            raise ValueError("Tool arguments must be an object")
        scope = await ctx.scope()
        result = await asyncio.to_thread(render_table, arguments, scope.workspace)
        if ctx.session is not None:
            await ctx.session.send(f"[table-image-plugin] 已生成表格图片：{result['path']}")
        return ToolResponse.completed(json.dumps(result, ensure_ascii=False))
