#!/usr/bin/env python3
import os
import sys
import json
import argparse
import subprocess
from pathlib import Path
from typing import List, Dict, Any, Optional
import re
from datetime import datetime

# 配置
IGNORE_DIRS = {'.git', 'node_modules', '__pycache__', 'venv', 'env', 'dist', 'build', 'target', '.idea', '.vscode'}
SUPPORTED_LANGUAGES = {
    'py': {'name': 'Python', 'extensions': ['.py']},
    'java': {'name': 'Java', 'extensions': ['.java']},
    'cpp': {'name': 'C/C++', 'extensions': ['.c', '.cpp', '.cc', '.h', '.hpp']},
    'go': {'name': 'Go', 'extensions': ['.go']},
    'js': {'name': 'JavaScript/TypeScript', 'extensions': ['.js', '.jsx', '.ts', '.tsx']}
}
IDE_CONFIG_FILES = {
    '.vscode': 'VS Code',
    '.idea': 'JetBrains IDE (IntelliJ/PyCharm/GoLand etc.)',
    'CMakeLists.txt': 'CMake',
    'Makefile': 'Make',
    'package.json': 'Node.js',
    'pyproject.toml': 'Python Project',
    'go.mod': 'Go Module',
    'pom.xml': 'Java Maven',
    'build.gradle': 'Java Gradle'
}

class ProjectScanner:
    def __init__(self, root_dir: str = '.', max_depth: int = 5):
        self.root_dir = Path(root_dir).resolve()
        self.max_depth = max_depth
        self.result: Dict[str, Any] = {
            'scan_time': datetime.now().isoformat(),
            'project_root': str(self.root_dir),
            'directory_tree': [],
            'git_info': {},
            'ide_configs': [],
            'code_stats': {},
            'code_structures': []
        }

    def scan_directory_tree(self, path: Optional[Path] = None, depth: int = 0) -> List[Dict]:
        if path is None:
            path = self.root_dir
        if depth > self.max_depth:
            return []
        items = []
        for item in path.iterdir():
            if item.name in IGNORE_DIRS:
                continue
            item_info = {
                'name': item.name,
                'type': 'directory' if item.is_dir() else 'file',
                'path': str(item.relative_to(self.root_dir))
            }
            if item.is_dir():
                item_info['children'] = self.scan_directory_tree(item, depth + 1)
            items.append(item_info)
        if depth == 0:
            self.result['directory_tree'] = items
        return items

    def scan_git_info(self):
        git_dir = self.root_dir / '.git'
        if not git_dir.exists():
            self.result['git_info']['exists'] = False
            return
        self.result['git_info']['exists'] = True
        try:
            def run_git_cmd(cmd: List[str]) -> str:
                return subprocess.check_output(cmd, cwd=self.root_dir, text=True, stderr=subprocess.DEVNULL).strip()
            self.result['git_info']['branch'] = run_git_cmd(['git', 'rev-parse', '--abbrev-ref', 'HEAD'])
            self.result['git_info']['last_commit'] = run_git_cmd(['git', 'log', '-1', '--pretty=%h %s (%an, %ad)'])
            self.result['git_info']['remote_url'] = run_git_cmd(['git', 'remote', 'get-url', 'origin'])
        except Exception as e:
            self.result['git_info']['error'] = str(e)

    def scan_ide_configs(self):
        configs = []
        for config_name, ide_name in IDE_CONFIG_FILES.items():
            config_path = self.root_dir / config_name
            if config_path.exists():
                configs.append({
                    'name': ide_name,
                    'config_file': config_name,
                    'type': 'directory' if config_path.is_dir() else 'file'
                })
        self.result['ide_configs'] = configs

    def extract_code_structure(self, file_path: Path, lang: str) -> Dict:
        structures = {'file': str(file_path.relative_to(self.root_dir)), 'language': lang, 'classes': [], 'functions': []}
        try:
            content = file_path.read_text(errors='ignore')
            if lang == 'Python':
                class_matches = re.findall(r'^class\s+(\w+)\s*[\(:]', content, re.MULTILINE)
                structures['classes'] = class_matches
                func_matches = re.findall(r'^def\s+(\w+)\s*\(', content, re.MULTILINE)
                structures['functions'] = func_matches
            elif lang == 'Java':
                class_matches = re.findall(r'(?:public|private|protected)?\s*class\s+(\w+)', content)
                structures['classes'] = class_matches
                func_matches = re.findall(r'(?:public|private|protected)?\s*\w+\s+(\w+)\s*\(', content)
                structures['functions'] = [f for f in func_matches if not f[0].isupper() and f != 'class']
            elif lang == 'C/C++':
                struct_matches = re.findall(r'struct\s+(\w+)', content)
                class_matches = re.findall(r'class\s+(\w+)', content)
                structures['classes'] = list(set(struct_matches + class_matches))
                func_matches = re.findall(r'(?:\w+\s+)+(\w+)\s*\([^)]*\)\s*{', content)
                structures['functions'] = [f for f in func_matches if f not in ['if', 'for', 'while', 'switch']]
            elif lang == 'Go':
                struct_matches = re.findall(r'type\s+(\w+)\s+struct', content)
                structures['classes'] = struct_matches
                func_matches = re.findall(r'func\s+(?:\([^)]*\)\s+)?(\w+)\s*\(', content)
                structures['functions'] = func_matches
            elif lang == 'JavaScript/TypeScript':
                class_matches = re.findall(r'class\s+(\w+)', content)
                structures['classes'] = class_matches
                func_matches = re.findall(r'(?:function\s+(\w+)|(\w+)\s*=\s*(?:async\s*)?\(|const\s+(\w+)\s*=\s*(?:async\s*)?\()', content)
                structures['functions'] = list(set([item for sublist in func_matches for item in sublist if item]))
        except Exception as e:
            structures['error'] = str(e)
        return structures

    def scan_code(self):
        stats = {lang['name']: {'file_count': 0, 'line_count': 0} for lang in SUPPORTED_LANGUAGES.values()}
        structures = []
        for root, _, files in os.walk(self.root_dir):
            root_path = Path(root)
            if any(ignore_dir in root_path.parts for ignore_dir in IGNORE_DIRS):
                continue
            for file in files:
                file_path = root_path / file
                suffix = file_path.suffix.lower()
                for lang_config in SUPPORTED_LANGUAGES.values():
                    if suffix in lang_config['extensions']:
                        lang_name = lang_config['name']
                        stats[lang_name]['file_count'] += 1
                        try:
                            stats[lang_name]['line_count'] += len(file_path.read_text(errors='ignore').splitlines())
                        except:
                            pass
                        struct = self.extract_code_structure(file_path, lang_name)
                        if struct['classes'] or struct['functions']:
                            structures.append(struct)
        self.result['code_stats'] = {k: v for k, v in stats.items() if v['file_count'] > 0}
        self.result['code_structures'] = structures

    def run_scan(self):
        self.scan_directory_tree()
        self.scan_git_info()
        self.scan_ide_configs()
        self.scan_code()

    def export_json(self, output_path: str):
        with open(output_path, 'w', encoding='utf-8') as f:
            json.dump(self.result, f, indent=2, ensure_ascii=False)
        print(f"JSON结果已导出到 {output_path}")

    def export_markdown(self, output_path: str):
        md = [
            f"# 项目扫描报告",
            f"> 扫描时间: {self.result['scan_time']}",
            f"> 项目根目录: {self.result['project_root']}\n",
            f"## 1. Git信息",
        ]
        if self.result['git_info'].get('exists'):
            md.extend([
                f"- 分支: {self.result['git_info'].get('branch', '未知')}",
                f"- 最新提交: {self.result['git_info'].get('last_commit', '未知')}",
                f"- 远程仓库: {self.result['git_info'].get('remote_url', '未知')}"
            ])
        else:
            md.append("- 非Git仓库")

        md.extend([
            "\n## 2. IDE/构建配置",
            *[f"- {conf['name']} ({conf['config_file']})" for conf in self.result['ide_configs']] if self.result['ide_configs'] else ["- 未检测到常见IDE配置"],
            "\n## 3. 代码统计"
        ])
        for lang, stat in self.result['code_stats'].items():
            md.append(f"- {lang}: {stat['file_count']} 个文件, {stat['line_count']} 行代码")

        md.extend([
            "\n## 4. 代码结构",
        ])
        for struct in self.result['code_structures']:
            md.extend([
                f"\n### {struct['file']} ({struct['language']})",
                f"- 类/结构体: {', '.join(struct['classes']) if struct['classes'] else '无'}",
                f"- 函数: {', '.join(struct['functions']) if struct['functions'] else '无'}"
            ])

        def render_tree(items: List[Dict], depth: int = 0) -> List[str]:
            lines = []
            indent = "  " * depth
            for item in items:
                prefix = "📂 " if item['type'] == 'directory' else "📄 "
                lines.append(f"{indent}- {prefix}{item['name']}")
                if item['type'] == 'directory' and 'children' in item:
                    lines.extend(render_tree(item['children'], depth + 1))
            return lines

        md.extend([
            "\n## 5. 目录结构",
            *render_tree(self.result['directory_tree'])
        ])

        with open(output_path, 'w', encoding='utf-8') as f:
            f.write('\n'.join(md))
        print(f"Markdown报告已导出到 {output_path}")

def main():
    parser = argparse.ArgumentParser(description='项目信息扫描工具')
    parser.add_argument('--root', '-r', default='.', help='项目根目录 (默认当前目录)')
    parser.add_argument('--output', '-o', default='project_scan', help='输出文件名 (不带后缀)')
    parser.add_argument('--format', '-f', choices=['json', 'md', 'all'], default='all', help='输出格式 (默认all)')
    parser.add_argument('--max-depth', '-d', type=int, default=5, help='目录树最大深度 (默认5)')
    args = parser.parse_args()

    scanner = ProjectScanner(args.root, args.max_depth)
    print("开始扫描项目...")
    scanner.run_scan()
    print("扫描完成，正在导出结果...")

    if args.format in ['json', 'all']:
        scanner.export_json(f"{args.output}.json")
    if args.format in ['md', 'all']:
        scanner.export_markdown(f"{args.output}.md")
    print("全部操作完成!")

if __name__ == '__main__':
    main()