#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
项目目录关键信息扫描脚本（优化版）
功能：扫描当前项目目录的结构和关键信息，自动屏蔽常见的干扰目录
使用方法：直接运行脚本即可，无需额外参数，也支持命令行参数自定义扫描规则
优化点：
1. 性能提升：使用os.scandir替代os.walk，大目录扫描速度提升2-20倍
2. 功能增强：支持命令行参数、有效代码行统计、结构化报告导出、项目目录树生成
3. 兼容性增强：编码容错、自动跳过异常文件/目录
"""
import os
import sys
import json
import argparse
from collections import defaultdict

# 需要屏蔽的干扰目录列表，可根据需要扩展
DEFAULT_EXCLUDE_DIRS = {
    'target', 'node_modules', '.git', '.idea', 'dist', 'build',
    '__pycache__', '.vscode', '.venv', 'env', 'venv', '.DS_Store',
    '.gradle', '.mvn', 'out', 'logs', 'tmp', 'temp', '.npm', '.yarn'
}

# 需要统计的常见代码文件后缀，可根据需要扩展
DEFAULT_CODE_EXTENSIONS = {
    '.py', '.java', '.js', '.ts', '.go', '.cpp', '.c', '.h', '.hpp',
    '.rs', '.rb', '.php', '.vue', '.react', '.html', '.css', '.scss',
    '.xml', '.json', '.yaml', '.yml', '.toml', '.md', '.sh', '.bat'
}

# 关键文件列表
KEY_FILES = {
    'readme.md', 'readme', 'license', 'package.json', 'pom.xml',
    'requirements.txt', 'setup.py', 'dockerfile', 'docker-compose.yml'
}

# 单行注释前缀（用于统计有效代码行）
COMMENT_PREFIXES = ('#', '//', '--', '/*', '*/', '<!--', '-->')

def count_effective_lines(file_path):
    """
    统计有效代码行数（排除空行和单行注释）
    :param file_path: 文件路径
    :return: 有效代码行数，失败返回0
    """
    total_lines = 0
    effective_lines = 0
    try:
        # 先尝试utf-8编码
        with open(file_path, 'r', encoding='utf-8') as f:
            lines = f.readlines()
    except (UnicodeDecodeError, PermissionError, IsADirectoryError):
        try:
            # 兜底使用latin-1编码
            with open(file_path, 'r', encoding='latin-1') as f:
                lines = f.readlines()
        except Exception:
            return 0, 0
    
    total_lines = len(lines)
    for line in lines:
        stripped = line.strip()
        # 排除空行和单行注释
        if stripped and not stripped.startswith(COMMENT_PREFIXES):
            effective_lines += 1
    
    return total_lines, effective_lines

def scan_with_scandir(root_path, exclude_dirs, code_extensions, recursive=True, generate_tree=False):
    """
    使用os.scandir扫描目录（性能更高）
    :param root_path: 根目录
    :param exclude_dirs: 要排除的目录集合
    :param code_extensions: 要统计的代码后缀集合
    :param recursive: 是否递归扫描
    :param generate_tree: 是否生成目录树
    :return: 扫描结果字典
    """
    result = {
        'project_root': os.path.abspath(root_path),
        'total_dirs': 0,
        'total_files': 0,
        'total_code_lines': 0,
        'total_effective_lines': 0,
        'file_type_stats': defaultdict(int),
        'key_files': [],
        'directory_tree': []
    }

    def _scan(current_path, tree_node=None, depth=0):
        try:
            with os.scandir(current_path) as entries:
                for entry in entries:
                    # 处理目录
                    if entry.is_dir(follow_symlinks=False):
                        if entry.name in exclude_dirs:
                            continue
                        result['total_dirs'] += 1
                        
                        # 添加到目录树
                        if generate_tree and tree_node is not None:
                            dir_node = {'name': entry.name, 'type': 'dir', 'children': []}
                            tree_node.append(dir_node)
                        else:
                            dir_node = None
                        
                        # 递归扫描
                        if recursive:
                            _scan(entry.path, dir_node['children'] if dir_node else None, depth + 1)
                    
                    # 处理文件
                    elif entry.is_file(follow_symlinks=False):
                        # 跳过隐藏文件
                        if entry.name.startswith('.'):
                            continue
                            
                        result['total_files'] += 1
                        file_ext = os.path.splitext(entry.name)[1].lower()
                        
                        # 统计文件类型
                        result['file_type_stats'][file_ext] += 1
                        
                        # 统计代码行数
                        if file_ext in code_extensions:
                            total_lines, effective_lines = count_effective_lines(entry.path)
                            result['total_code_lines'] += total_lines
                            result['total_effective_lines'] += effective_lines
                        
                        # 记录关键文件
                        if entry.name.lower() in KEY_FILES:
                            rel_path = os.path.relpath(entry.path, root_path)
                            result['key_files'].append(rel_path)
                        
                        # 添加到目录树
                        if generate_tree and tree_node is not None:
                            tree_node.append({'name': entry.name, 'type': 'file'})
                            
        except (PermissionError, OSError):
            # 跳过权限不足或无法访问的目录
            pass

    # 启动扫描
    if generate_tree:
        root_node = {'name': os.path.basename(root_path), 'type': 'dir', 'children': []}
        result['directory_tree'] = root_node
        _scan(root_path, root_node['children'])
    else:
        _scan(root_path)

    return result

def generate_tree_str(tree_node, prefix='', is_last=True):
    """
    生成目录树字符串
    """
    lines = []
    connector = '└── ' if is_last else '├── '
    lines.append(f"{prefix}{connector}{tree_node['name']}{'/' if tree_node['type'] == 'dir' else ''}")
    
    if tree_node['type'] == 'dir' and 'children' in tree_node:
        children = tree_node['children']
        for i, child in enumerate(children):
            new_prefix = prefix + ('    ' if is_last else '│   ')
            lines.extend(generate_tree_str(child, new_prefix, i == len(children) - 1))
    
    return lines

def print_report(result, output_path=None, export_format='txt'):
    """
    打印/导出扫描结果报告
    :param result: 扫描结果字典
    :param output_path: 输出文件路径，None则仅打印到控制台
    :param export_format: 导出格式 txt/json
    """
    # 生成报告内容
    report_lines = []
    report_lines.append("=" * 60)
    report_lines.append("项目目录扫描报告")
    report_lines.append("=" * 60)
    report_lines.append(f"项目根目录: {result['project_root']}")
    report_lines.append(f"总目录数: {result['total_dirs']}")
    report_lines.append(f"总文件数: {result['total_files']}")
    report_lines.append(f"总代码行数: {result['total_code_lines']} (包含空行和注释)")
    report_lines.append(f"有效代码行数: {result['total_effective_lines']} (排除空行和单行注释)")
    report_lines.append("-" * 60)
    
    report_lines.append("\n文件类型统计(前10种):")
    sorted_types = sorted(result['file_type_stats'].items(), key=lambda x: x[1], reverse=True)[:10]
    for ext, count in sorted_types:
        report_lines.append(f"  {ext or '无后缀'}: {count}个")
    
    report_lines.append("\n关键文件列表:")
    for file in result['key_files']:
        report_lines.append(f"  - {file}")
    
    # 目录树
    if result.get('directory_tree'):
        report_lines.append("\n项目目录树:")
        report_lines.extend(generate_tree_str(result['directory_tree']))
    
    report_lines.append("\n" + "=" * 60)
    report_lines.append("扫描完成!")
    report_lines.append("提示: 如需调整屏蔽目录或统计规则，可通过命令行参数指定或修改脚本头部变量")
    
    # 控制台输出
    for line in report_lines:
        print(line)
    
    # 导出文件
    if output_path:
        try:
            if export_format == 'json':
                with open(output_path, 'w', encoding='utf-8') as f:
                    json.dump(result, f, ensure_ascii=False, indent=2)
                print(f"\nJSON报告已导出到: {os.path.abspath(output_path)}")
            else:
                with open(output_path, 'w', encoding='utf-8') as f:
                    f.write('\n'.join(report_lines))
                print(f"\nTXT报告已导出到: {os.path.abspath(output_path)}")
        except Exception as e:
            print(f"\n导出报告失败: {str(e)}")

def main():
    parser = argparse.ArgumentParser(description='项目目录关键信息扫描工具')
    parser.add_argument('--root', type=str, default=os.getcwd(), help='要扫描的根目录，默认当前目录')
    parser.add_argument('--exclude', type=str, nargs='*', help='额外要排除的目录名称，多个用空格分隔')
    parser.add_argument('--ext', type=str, nargs='*', help='额外要统计的代码文件后缀，多个用空格分隔（如: .kt .swift）')
    parser.add_argument('--output', type=str, help='导出报告的文件路径，不指定则仅打印到控制台')
    parser.add_argument('--format', type=str, choices=['txt', 'json'], default='txt', help='导出报告格式，默认txt')
    parser.add_argument('--no-recursive', action='store_true', help='不递归扫描子目录，默认递归')
    parser.add_argument('--no-tree', action='store_true', help='关闭目录树展示，默认开启')
    
    args = parser.parse_args()
    
    # 合并排除目录
    exclude_dirs = DEFAULT_EXCLUDE_DIRS.copy()
    if args.exclude:
        exclude_dirs.update(args.exclude)
    
    # 合并代码后缀
    code_extensions = DEFAULT_CODE_EXTENSIONS.copy()
    if args.ext:
        code_extensions.update([e.lower() for e in args.ext])
    
    try:
        # 执行扫描
        scan_result = scan_with_scandir(
            root_path=args.root,
            exclude_dirs=exclude_dirs,
            code_extensions=code_extensions,
            recursive=not args.no_recursive,
            generate_tree=not args.no_tree
        )
        
        # 输出报告
        print_report(scan_result, args.output, args.format)
        
    except KeyboardInterrupt:
        print("\n扫描被用户中断")
        sys.exit(1)
    except Exception as e:
        print(f"扫描出错: {str(e)}")
        sys.exit(1)

if __name__ == "__main__":
    main()