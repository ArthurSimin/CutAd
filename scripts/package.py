"""Package a native CutAd release with dependency notices."""
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import zipfile

parser = argparse.ArgumentParser()
parser.add_argument("target")
parser.add_argument("platform", choices=("linux-x86_64", "windows-x86_64"))
parser.add_argument("binary", type=Path)
parser.add_argument("version")
args = parser.parse_args()
assert re.fullmatch(r"\d+\.\d+\.\d+", args.version)
output = Path("dist")
output.mkdir(exist_ok=True)

def cargo(*flags):
    return subprocess.check_output(["cargo", *flags, "--locked"], text=True)

metadata = json.loads(cargo("metadata", "--format-version=1", "--filter-platform", args.target))
packages = {(p["name"], p["version"]): p for p in metadata["packages"]}
graph = cargo("tree", "--target", args.target, "-e", "normal", "--prefix", "none", "--format", "{p}")
active = set(re.findall(r"^([^\s]+) v([^\s]+)", graph, re.M))
notice = re.compile(r"^(license|licence|copying|copyright|notice|ofl|ufl|hack-regular\.txt)", re.I)

with tempfile.TemporaryDirectory() as scratch:
    stage = Path(scratch) / f"CutAd-{args.version}-{args.platform}"
    stage.mkdir()
    for name in ("README.md", "LICENSE", "FEATURES.md", "CAPCUT_WORKFLOWS.md", "SETUP_EVIDENCE.md"):
        shutil.copy2(name, stage / name)
    for directory in ("docs", "assets"):
        shutil.copytree(directory, stage / directory, ignore=shutil.ignore_patterns("*.ttf"))
    shutil.copy2(args.binary, stage / args.binary.name)
    lines = ["Dependency notices and corresponding upstream source", ""]
    for name, version in sorted(active - {("cutad", args.version)}):
        package = packages[name, version]
        source = Path(package["manifest_path"]).parent
        lines.extend([
            f"{name} {version}: {package['license'] or package['license_file']}",
            f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
        ])
        files = set(source.iterdir())
        for sub in ("licenses", "LICENSES", "fonts"):
            if (source / sub).is_dir():
                files.update((source / sub).iterdir())
        if package["license_file"]:
            files.add(source / package["license_file"])
        for file in files:
            if file.is_file() and (notice.match(file.name) or file.parent.name in ("licenses", "LICENSES")):
                dest = stage / "third-party" / f"{name}-{version}" / file.relative_to(source)
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(file, dest)
    (stage / "DEPENDENCIES.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
    if args.platform.startswith("windows"):
        with zipfile.ZipFile(output / f"{stage.name}.zip", "w", zipfile.ZIP_DEFLATED) as archive:
            for file in sorted(stage.rglob("*")):
                if file.is_file():
                    archive.write(file, file.relative_to(stage.parent))
    else:
        with tarfile.open(output / f"{stage.name}.tar.gz", "w:gz") as archive:
            archive.add(stage, arcname=stage.name)
