import os

def main():
    docs_dir = "./"
    site_file = os.path.join(docs_dir, "site.txt")
    
    paths = []
    for root, dirs, files in os.walk(docs_dir):
        # Skip the .git or other hidden directories if needed, but we'll list all here
        for name in dirs + files:
            full_path = os.path.join(root, name)
            rel_path = os.path.relpath(full_path, docs_dir)
            # Skip the script itself and the site.txt file if we don't want them included
            # But according to "all files and directories", we can include everything.
            paths.append(rel_path)
            
    # Remove duplicates if any and sort
    paths = sorted(list(set(paths)))
    
    with open(site_file, "w", encoding="utf-8") as f:
        for p in paths:
            f.write(p + "\n")
            
    print(f"Successfully wrote {len(paths)} entries to {site_file}")

if __name__ == "__main__":
    main()
