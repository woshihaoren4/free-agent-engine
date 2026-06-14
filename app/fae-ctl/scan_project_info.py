#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
项目目录关键信息扫描脚本
功能：扫描当前项目目录的结构和关键信息，自动屏蔽常见的干扰目录
使用方法：直接运行脚本即可，无需额外参数
"""
import os
import sys
from collections import defaultdict

# 需要屏蔽的干扰目录列表，可根据需要扩展
EXCLUDE_DIRS = {
    'target', 'node_modules', '.git', '.idea', 'dist', 'build',
    '__pycache__', '.vscode', '.venv', 'env', 'venv', '.DS_Store',
    '.gradle', '.mvn', 'out', 'logs', 'tmp', 'temp', '.npm', '.yarn'
}

# 需要统计的常见代码文件后缀，可根据需要扩展
CODE_EXTENSIONS = {
    '.py', '.java', '.js', '.ts', '.go', '.cpp', '.c', '.h', '.hpp',
    '.rs', '.rb', '.php', '.vue', '.react', '.html', '.css', '.scss',
    '.xml', '.json', '.yaml', '.yml', '.toml', '.md', '.sh', '.bat'
}

def scan_project(root_path=None):
    """
    扫描项目目录
    :param root_path: 要扫描的根目录，默认为当前脚本所在目录
    :return: 扫描结果字典
    """
    if root_path is None:
        root_path = os.getcwd()
    
    result = {
        'project_root': os.path.abspath(root_path),
        'total_dirs': 0,
        'total_files': 0,
        'total_code_lines': 0,
        'file_type_stats': defaultdict(int),
        'key_files': [],
    }

    for root, dirs, files in os.walk(root_path):
        # 过滤掉需要排除的目录（修改dirs列表会影响os.walk的遍历）
        dirs[:] = [d for d in dirs if d not in EXCLUDE_DIRS]
        
        # 统计目录数量
        result['total_dirs'] += len(dirs)
        
        for file in files:
            # 跳过隐藏文件（可选，根据需要调整）
            if file.startswith('.'):
                continue
                
            result['total_files'] += 1
            file_path = os.path.join(root, file)
            file_ext = os.path.splitext(file)[1].lower()
            
            # 统计文件类型
            result['file_type_stats'][file_ext] += 1
            
            # 统计代码行数
            if file_ext in CODE_EXTENSIONS:
                try:
                    with open(file_path, 'r', encoding='utf-8') as f:
                        lines = f.readlines()
                        # 简单统计，不排除空行和注释
                        result['total_code_lines'] += len(lines)
                except (UnicodeDecodeError, PermissionError):
                    # 忽略无法读取的文件
                    pass
            
            # 记录关键文件
            if file.lower() in ['readme.md', 'readme', 'license', 'package.json', 'pom.xml', 'requirements.txt', 'setup.py', 'dockerfile', 'docker-compose.yml']:
                rel_path = os.path.relpath(file_path, root_path)
                result['key_files'].append(rel_path)

    return result

def print_report(result):
    """
    打印扫描结果报告
    :param result: scan_project返回的结果字典
    """
    print("=" * 60)
    print("项目目录扫描报告")
    print("=" * 60)
    print(f"项目根目录: {result['project_root']}")
    print(f"总目录数: {result['total_dirs']}")
    print(f"总文件数: {result['total_files']}")
    print(f"总代码行数: {result['total_code_lines']} (粗略统计)")
    print("-" * 60)
    
    print("\n文件类型统计(前10种):")
    # 按数量排序
    sorted_types = sorted(result['file_type_stats'].items(), key=lambda x: x[1], reverse=True)[:10]
    for ext, count in sorted_types:
        print(f"  {ext or '无后缀'}: {count}个")
    
    print("\n关键文件列表:")
    for file in result['key_files']:
        print(f"  - {file}")
    
    print("\n" + "=" * 60)
    print("扫描完成!")
    print("提示: 如需调整屏蔽目录或统计规则，请修改脚本头部的EXCLUDE_DIRS和CODE_EXTENSIONS变量")

if __name__ == "__main__":
    try:
        scan_result = scan_project()
        print_report(scan_result)
    except KeyboardInterrupt:
        print("\n扫描被用户中断")
        sys.exit(1)
    except Exception as e:
        print(f"扫描出错: {str(e)}")
        sys.exit(1)