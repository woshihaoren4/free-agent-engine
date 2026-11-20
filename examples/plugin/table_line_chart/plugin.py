"""Generate a line chart; forward other tools along the shared tool chain."""
import asyncio
import json
import math
from pathlib import Path
from typing import Any

from PIL import Image, ImageDraw, ImageFont
from fae_plugin import ChainToolsHook, Context, HookNext, ToolRequest, ToolResponse


def chart_data(arguments: dict[str, Any]) -> tuple[list[str | int | float], list[float]]:
    x, y = arguments.get("x"), arguments.get("y")
    if not isinstance(x, list) or not isinstance(y, list) or not 1 <= len(x) <= 100:
        raise ValueError("x and y must be arrays with 1..100 paired points")
    if len(x) != len(y):
        raise ValueError("x and y must have equal lengths")
    if any(type(value) not in (str, int, float) for value in x):
        raise ValueError("x values must be strings or numbers")
    if any(type(value) not in (int, float) for value in y):
        raise ValueError("y values must be finite numbers for a line chart")
    try:
        values = [float(value) for value in y]
        numeric_x = [float(value) for value in x if type(value) in (int, float)]
    except OverflowError as error:
        raise ValueError("x/y numbers are outside the supported range") from error
    if not all(math.isfinite(value) for value in values + numeric_x):
        raise ValueError("x/y numbers must be finite")
    labels = [arguments.get("title", "X / Y Line Chart"),
              arguments.get("x_label", "X"), arguments.get("y_label", "Y"),
              *[str(value) for value in x]]
    if any(not isinstance(label, str) or len(label) > 120 or "\n" in label or "\r" in label
           for label in labels):
        raise ValueError("Each title/label must be a single line of at most 120 characters")
    return x, values


def render_line_chart(arguments: dict[str, Any], workspace: str | None) -> dict[str, Any]:
    x, y = chart_data(arguments)
    path = arguments.get("path")
    if not isinstance(path, str) or not path.strip():
        raise ValueError("path must be a nonempty PNG path")
    output = Path(path).expanduser()
    if not output.is_absolute():
        if workspace is None:
            raise ValueError("A relative path requires an Agent workspace; otherwise use an absolute path")
        output = Path(workspace) / output
    output = output.resolve()
    if output.suffix.lower() != ".png":
        raise ValueError("path must end with .png")
    font_path = arguments.get("font_path")
    if font_path is not None:
        if not isinstance(font_path, str) or not Path(font_path).is_absolute():
            raise ValueError("font_path must be an absolute TTF/OTF/TTC path")
        font = ImageFont.truetype(font_path, size=20)
    else:
        font = ImageFont.load_default(size=20)

    # Normalize before computing the range so large finite values do not overflow.
    scale = max(abs(value) for value in y) or 1.0
    normalized = [value / scale for value in y]
    low, high = min(normalized), max(normalized)
    padding = (high - low) * 0.1 or 0.1
    low, high = low - padding, high + padding
    width, height = 1000, 600
    left, top, right, bottom = 150, 90, width - 50, height - 100
    numeric = all(type(value) in (int, float) for value in x)
    positions = [float(value) for value in x] if numeric else list(range(len(x)))
    x_scale = max(abs(value) for value in positions) or 1.0
    positions = [value / x_scale for value in positions]
    x_min, x_max = min(positions), max(positions)
    points = [
        (left + (position - x_min) / (x_max - x_min) * (right - left)
         if x_max != x_min else (left + right) / 2,
         bottom - (value - low) / (high - low) * (bottom - top))
        for position, value in zip(positions, normalized)
    ]
    with Image.new("RGB", (width, height), "#ffffff") as image:
        draw = ImageDraw.Draw(image)
        draw.text((left, 25), arguments.get("title", "X / Y Line Chart"),
                  font=font, fill="#172554")
        draw.text((left, 60), arguments.get("y_label", "Y"), font=font, fill="#475569")
        for tick in range(6):
            fraction = tick / 5
            pixel_y = bottom - fraction * (bottom - top)
            draw.line((left, pixel_y, right, pixel_y), fill="#e2e8f0", width=1)
            value = (low + fraction * (high - low)) * scale
            label = f"{value:.4g}" if math.isfinite(value) else ""
            draw.text((left - 12, pixel_y), label, anchor="rm", font=font, fill="#475569")
        draw.line((left, top, left, bottom, right, bottom), fill="#64748b", width=2)
        if len(points) > 1:
            draw.line(points, fill="#2563eb", width=3)
        label_indices = {round(index * (len(x) - 1) / min(7, len(x) - 1))
                         for index in range(min(8, len(x)))} if len(x) > 1 else {0}
        for index, (pixel_x, pixel_y) in enumerate(points):
            draw.ellipse((pixel_x - 4, pixel_y - 4, pixel_x + 4, pixel_y + 4),
                         fill="#2563eb", outline="#ffffff")
            if index in label_indices:
                label = str(x[index])
                if len(label) > 16:
                    label = label[:13] + "..."
                draw.text((pixel_x, bottom + 16), label, anchor="mt",
                          font=font, fill="#475569")
        draw.text(((left + right) / 2, height - 30), arguments.get("x_label", "X"),
                  anchor="mm", font=font, fill="#475569")
        output.parent.mkdir(parents=True, exist_ok=True)
        image.save(output, format="PNG")
    return {"path": str(output), "points": len(x), "width": width, "height": height}


class Table2Tool(ChainToolsHook):
    async def desc(self, ctx: Context, tool_name: str, *,
                   next: HookNext[str, dict[str, Any]]) -> dict[str, Any]:
        if tool_name != f"{self.channel()}__line_chart":
            return await next(tool_name)
        return {
            "name": tool_name,
            "description": (
                "折线图生成：将等长 x/y 数据绘制为折线图 PNG，展示趋势并返回真实文件路径。"
                "x 为文字时等距排列，为数字时按数值定位；连线保持输入顺序。y 必须为有限数字。"
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "折线图 .png 路径；相对路径基于 Agent workspace"},
                    "x": {"type": "array", "minItems": 1, "maxItems": 100,
                          "items": {"type": ["string", "number"]}, "description": "横轴文字或有限数字"},
                    "y": {"type": "array", "minItems": 1, "maxItems": 100,
                          "items": {"type": "number"}, "description": "纵轴有限数字，长度必须与 x 相同"},
                    "title": {"type": "string", "description": "图片标题，默认 X / Y Line Chart"},
                    "x_label": {"type": "string", "description": "横轴名称，默认 X"},
                    "y_label": {"type": "string", "description": "纵轴名称，默认 Y"},
                    "font_path": {"type": "string", "description": "可选绝对字体路径；中文需使用支持中文的字体"},
                },
                "required": ["path", "x", "y"],
                "additionalProperties": False,
            },
        }

    async def exec(self, ctx: Context, request: ToolRequest, *,
                   next: HookNext[ToolRequest, ToolResponse]) -> ToolResponse:
        if request.tool_name != f"{self.channel()}__line_chart":
            return await next(request)
        arguments = request.arguments_json()
        if not isinstance(arguments, dict):
            raise ValueError("Tool arguments must be an object")
        scope = await ctx.scope()
        result = await asyncio.to_thread(render_line_chart, arguments, scope.workspace)
        if ctx.session is not None:
            await ctx.session.send(f"[table2-plugin] 已生成折线图：{result['path']}")
        return ToolResponse.completed(json.dumps(result, ensure_ascii=False))
