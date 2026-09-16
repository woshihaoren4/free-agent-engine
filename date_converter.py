#!/usr/bin/env python3
"""
日期转换工具
支持多种日期格式之间的互相转换，自动识别输入格式。

用法: python date_converter.py <输入日期> <目标格式>
目标格式: iso(YYYY-MM-DD), slash(YYYY/MM/DD), dash-eu(DD-MM-YYYY),
         slash-us(MM/DD/YYYY), chinese(YYYY年MM月DD日), timestamp
"""
import sys
import re
from datetime import datetime


# 格式定义: (模式字符串, 匹配正则, 输出格式)
DATE_FORMATS = [
    # ISO: YYYY-MM-DD
    ("iso", re.compile(r"^(\d{4})-(\d{1,2})-(\d{1,2})$"), "%Y-%m-%d"),
    # Slash: YYYY/MM/DD
    ("slash", re.compile(r"^(\d{4})/(\d{1,2})/(\d{1,2})$"), "%Y/%m/%d"),
    # Chinese: YYYY年MM月DD日
    ("chinese", re.compile(r"^(\d{4})年(\d{1,2})月(\d{1,2})日$"), "%Y年%m月%d日"),
    # Dash-EU: DD-MM-YYYY
    ("dash-eu", re.compile(r"^(\d{1,2})-(\d{1,2})-(\d{4})$"), "%d-%m-%Y"),
    # Slash-US: MM/DD/YYYY
    ("slash-us", re.compile(r"^(\d{1,2})/(\d{1,2})/(\d{4})$"), "%m/%d/%Y"),
]

# 目标格式名 -> strftime 格式字符串
OUTPUT_FORMATS = {
    "iso": "%Y-%m-%d",
    "slash": "%Y/%m/%d",
    "dash-eu": "%d-%m-%Y",
    "slash-us": "%m/%d/%Y",
    "chinese": "%Y年%m月%d日",
}

VALID_TARGETS = list(OUTPUT_FORMATS.keys()) + ["timestamp"]


def parse_date(input_str: str) -> datetime:
    """自动识别日期格式并解析为 datetime 对象"""
    input_str = input_str.strip()

    # 尝试解析 Unix 时间戳（纯数字）
    if input_str.isdigit():
        try:
            ts = int(input_str)
            return datetime.fromtimestamp(ts)
        except (ValueError, OSError) as e:
            raise ValueError(f"无效的时间戳: {input_str}") from e

    # 依次尝试各格式
    for name, pattern, fmt in DATE_FORMATS:
        if pattern.match(input_str):
            try:
                return datetime.strptime(input_str, fmt)
            except ValueError:
                continue

    raise ValueError(
        f"无法识别的日期格式: {input_str}\n"
        f"支持的输入格式: YYYY-MM-DD, YYYY/MM/DD, DD-MM-YYYY, "
        f"MM/DD/YYYY, YYYY年MM月DD日, Unix时间戳"
    )


def convert_date(input_str: str, target_fmt: str) -> str:
    """将输入日期转换为目标格式"""
    dt = parse_date(input_str)

    if target_fmt == "timestamp":
        return str(int(dt.timestamp()))

    if target_fmt not in OUTPUT_FORMATS:
        raise ValueError(
            f"不支持的目标格式: {target_fmt}\n"
            f"支持的目标格式: {', '.join(VALID_TARGETS)}"
        )

    return dt.strftime(OUTPUT_FORMATS[target_fmt])


def main():
    if len(sys.argv) != 3:
        print(__doc__)
        print(f"支持的目标格式: {', '.join(VALID_TARGETS)}")
        sys.exit(1)

    input_date = sys.argv[1]
    target_fmt = sys.argv[2].lower()

    try:
        result = convert_date(input_date, target_fmt)
        print(result)
    except ValueError as e:
        print(f"错误: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
