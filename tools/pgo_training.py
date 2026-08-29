#!/usr/bin/env python3
"""Stage and execute the release-PGO corpus under a bounded, exact-output policy."""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import threading
from pathlib import Path, PurePosixPath
from typing import Mapping, Sequence


POLICY_ID = (
    "zipp-pgo-runner-v1;exclusive-readonly-stage;external-code-off;"
    "timeout=30s;stdout<=4096;combined-output<=8192;output=manifest;"
    "one-profraw-per-input;explicit-hashed-profile-merge;atomic-publish"
)
TIMEOUT_SECONDS = 30.0
STDOUT_LIMIT = 4096
COMBINED_OUTPUT_LIMIT = 8192
REPARSE_POINT = 0x400
SHA256_RE = re.compile(r"[0-9a-f]{64}")


class TrainingError(ValueError):
    """The staged corpus or one bounded training execution is invalid."""


@dataclasses.dataclass(frozen=True)
class ExpectedCase:
    path: str
    stdout: bytes
    stderr: bytes


@dataclasses.dataclass(frozen=True)
class ProcessResult:
    returncode: int
    stdout: bytes
    stderr: bytes
    timed_out: bool
    output_exceeded: bool


def _is_reparse(st: os.stat_result) -> bool:
    return bool(getattr(st, "st_file_attributes", 0) & REPARSE_POINT)


def _lstat_plain(path: Path, *, kind: str) -> os.stat_result:
    try:
        info = path.lstat()
    except OSError as exc:
        raise TrainingError(f"missing {kind}: {path}") from exc
    if stat.S_ISLNK(info.st_mode) or _is_reparse(info):
        raise TrainingError(f"{kind} is a symlink or reparse point: {path}")
    expected = stat.S_ISREG if kind == "file" else stat.S_ISDIR
    if not expected(info.st_mode):
        raise TrainingError(f"{kind} has the wrong filesystem type: {path}")
    if hasattr(os, "getuid") and info.st_uid != os.getuid():
        raise TrainingError(f"{kind} is not owned by the current user: {path}")
    return info


def _identity(info: os.stat_result) -> tuple[int, int]:
    return (info.st_dev, info.st_ino)


def _plain_lexical_directory(path: Path) -> tuple[Path, os.stat_result]:
    """Resolve a directory only after proving its lexical path has no redirects."""

    lexical = Path(os.path.abspath(path))
    info = _lstat_plain(lexical, kind="directory")
    resolved = lexical.resolve(strict=True)
    if os.path.normcase(str(resolved)) != os.path.normcase(str(lexical)):
        raise TrainingError(f"directory path contains a symlink or reparse redirect: {path}")
    return resolved, info


def _require_identity(path: Path, expected: os.stat_result, *, kind: str) -> None:
    if _identity(_lstat_plain(path, kind=kind)) != _identity(expected):
        raise TrainingError(f"filesystem object was replaced during operation: {path}")


def check_plain_path(path: Path, *, kind: str, empty: bool = False) -> None:
    _lstat_plain(path, kind=kind)
    if kind == "directory" and empty and any(path.iterdir()):
        raise TrainingError(f"directory is not empty: {path}")


def _sha256_path(path: Path) -> str:
    digest = hashlib.sha256()
    content = _read_plain_file(path)
    digest.update(content)
    return digest.hexdigest()


def _canonical_relative(raw: str) -> PurePosixPath:
    pure = PurePosixPath(raw)
    if (
        pure.is_absolute()
        or "\\" in raw
        or raw.startswith("//")
        or (len(raw) >= 2 and raw[0].isalpha() and raw[1] == ":")
        or any(part in ("", ".", "..") for part in pure.parts)
        or raw != pure.as_posix()
        or any(ch in raw for ch in "\r\n\0")
    ):
        raise TrainingError(f"non-canonical corpus path: {raw!r}")
    return pure


def _read_plain_file(path: Path) -> bytes:
    before = _lstat_plain(path, kind="file")
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0)
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise TrainingError(f"cannot securely open source file: {path}") from exc
    try:
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode):
            raise TrainingError(f"opened source is not a regular file: {path}")
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            raise TrainingError(f"source changed while it was opened: {path}")
        opened_identity = _identity(opened)
        opened_shape = (
            opened.st_size,
            getattr(opened, "st_mtime_ns", None),
            getattr(opened, "st_ctime_ns", None),
        )
        chunks: list[bytes] = []
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            chunks.append(chunk)
        after = os.fstat(descriptor)
        after_shape = (
            after.st_size,
            getattr(after, "st_mtime_ns", None),
            getattr(after, "st_ctime_ns", None),
        )
        if _identity(after) != opened_identity or after_shape != opened_shape:
            raise TrainingError(f"source changed while it was read: {path}")
        _require_identity(path, before, kind="file")
        return b"".join(chunks)
    finally:
        os.close(descriptor)


def stage_named_files(
    *, destination: Path, files: Mapping[str, Path], verbose: bool = True
) -> None:
    """Copy named plain files into one exclusive, sealed staging tree."""

    check_plain_path(destination, kind="directory", empty=True)
    if not files:
        raise TrainingError("staging file list must be non-empty")
    canonical: list[tuple[PurePosixPath, Path]] = []
    seen: set[str] = set()
    for raw, source in files.items():
        pure = _canonical_relative(raw)
        if pure.as_posix() in seen:
            raise TrainingError(f"duplicate staging path: {raw}")
        seen.add(pure.as_posix())
        canonical.append((pure, source))

    for pure, source in canonical:
        content = _read_plain_file(source)
        target = destination.joinpath(*pure.parts)
        target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0)
        descriptor = os.open(target, flags, 0o400)
        try:
            view = memoryview(content)
            while view:
                written = os.write(descriptor, view)
                view = view[written:]
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        os.chmod(target, stat.S_IREAD)
        if verbose:
            print(
                f"staged {pure.as_posix()} "
                f"sha256={hashlib.sha256(content).hexdigest()}"
            )
    directories = sorted(
        (path for path in destination.rglob("*") if path.is_dir()),
        key=lambda path: len(path.parts),
        reverse=True,
    )
    for directory in directories:
        _lstat_plain(directory, kind="directory")
        os.chmod(directory, stat.S_IREAD | stat.S_IEXEC)
    os.chmod(destination, stat.S_IREAD | stat.S_IEXEC)


def stage_files(*, root: Path, destination: Path, paths: Sequence[str]) -> None:
    root = root.resolve(strict=True)
    if not paths or len(paths) != len(set(paths)):
        raise TrainingError("staging file list must be non-empty and unique")
    files: dict[str, Path] = {}
    for raw in paths:
        pure = _canonical_relative(raw)
        source = root.joinpath(*pure.parts)
        try:
            source.resolve(strict=True).relative_to(root)
        except (OSError, ValueError) as exc:
            raise TrainingError(f"source escapes or is missing: {raw}") from exc
        component = root
        for part in pure.parts[:-1]:
            component /= part
            _lstat_plain(component, kind="directory")
        files[raw] = source
    stage_named_files(destination=destination, files=files)


def load_expected_manifest(path: Path) -> tuple[ExpectedCase, ...]:
    content = _read_plain_file(path)

    def unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
        result: dict[str, object] = {}
        for key, value in pairs:
            if key in result:
                raise TrainingError(f"duplicate expected-output JSON key: {key!r}")
            result[key] = value
        return result

    try:
        raw = json.loads(content.decode("utf-8"), object_pairs_hook=unique_object)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise TrainingError(f"invalid expected-output manifest: {path}") from exc
    if not isinstance(raw, dict) or set(raw) != {"schema_version", "cases"}:
        raise TrainingError("expected-output manifest has unknown or missing keys")
    if (
        type(raw["schema_version"]) is not int
        or raw["schema_version"] != 1
        or not isinstance(raw["cases"], list)
    ):
        raise TrainingError("unsupported expected-output manifest schema")
    cases: list[ExpectedCase] = []
    seen: set[str] = set()
    for entry in raw["cases"]:
        if not isinstance(entry, dict) or set(entry) != {"path", "stdout", "stderr"}:
            raise TrainingError("expected-output case has unknown or missing keys")
        case_path = entry["path"]
        stdout = entry["stdout"]
        stderr = entry["stderr"]
        if not all(isinstance(value, str) for value in (case_path, stdout, stderr)):
            raise TrainingError("expected-output path/stdout/stderr must be strings")
        _canonical_relative(case_path)
        if case_path in seen:
            raise TrainingError(f"duplicate expected-output path: {case_path}")
        seen.add(case_path)
        encoded_stdout = stdout.encode("utf-8")
        encoded_stderr = stderr.encode("utf-8")
        if len(encoded_stdout) > STDOUT_LIMIT:
            raise TrainingError(f"expected stdout exceeds policy limit: {case_path}")
        if len(encoded_stdout) + len(encoded_stderr) > COMBINED_OUTPUT_LIMIT:
            raise TrainingError(f"expected output exceeds policy limit: {case_path}")
        cases.append(ExpectedCase(case_path, encoded_stdout, encoded_stderr))
    if not cases:
        raise TrainingError("expected-output manifest is empty")
    return tuple(cases)


def run_bounded(command: Sequence[str], *, environment: dict[str, str]) -> ProcessResult:
    process = subprocess.Popen(
        list(command),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
    )
    assert process.stdout is not None and process.stderr is not None
    captured = [bytearray(), bytearray()]
    lock = threading.Lock()
    output_exceeded = threading.Event()

    def read_stream(index: int, stream: object) -> None:
        while True:
            chunk = stream.read(4096)  # type: ignore[attr-defined]
            if not chunk:
                return
            with lock:
                remaining = COMBINED_OUTPUT_LIMIT + 1 - sum(map(len, captured))
                if remaining > 0:
                    captured[index].extend(chunk[:remaining])
                if sum(map(len, captured)) > COMBINED_OUTPUT_LIMIT:
                    output_exceeded.set()
            if output_exceeded.is_set():
                try:
                    process.kill()
                except OSError:
                    pass
                return

    threads = [
        threading.Thread(target=read_stream, args=(0, process.stdout), daemon=True),
        threading.Thread(target=read_stream, args=(1, process.stderr), daemon=True),
    ]
    for thread in threads:
        thread.start()
    timed_out = False
    try:
        returncode = process.wait(timeout=TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        timed_out = True
        process.kill()
        returncode = process.wait(timeout=5)
    for thread in threads:
        thread.join(timeout=5)
    process.stdout.close()
    process.stderr.close()
    return ProcessResult(
        returncode=returncode,
        stdout=bytes(captured[0]),
        stderr=bytes(captured[1]),
        timed_out=timed_out,
        output_exceeded=output_exceeded.is_set(),
    )


def _profile_entries(directory: Path) -> dict[str, Path]:
    entries: dict[str, Path] = {}
    for path in directory.iterdir():
        _lstat_plain(path, kind="file")
        if not path.name.endswith(".profraw") or any(ch in path.name for ch in "\r\n\0"):
            raise TrainingError(f"unexpected profile entry: {path.name!r}")
        entries[path.name] = path
    return entries


def run_training(
    *,
    root: Path,
    binary: Path,
    manifest: Path,
    profile_dir: Path,
    profile_list: Path,
    inputs: Sequence[str],
) -> None:
    root = root.resolve(strict=True)
    _lstat_plain(binary, kind="file")
    check_plain_path(profile_dir, kind="directory", empty=True)
    try:
        manifest.resolve(strict=True).relative_to(root)
    except (OSError, ValueError) as exc:
        raise TrainingError("expected-output manifest must be inside the stage") from exc
    cases = load_expected_manifest(manifest)
    if tuple(case.path for case in cases) != tuple(inputs):
        raise TrainingError("expected-output manifest order does not match training inputs")
    if profile_list.exists() or profile_list.is_symlink():
        raise TrainingError(f"profile list destination already exists: {profile_list}")

    profile_files: list[Path] = []
    for index, case in enumerate(cases):
        pure = _canonical_relative(case.path)
        staged = root.joinpath(*pure.parts)
        before_digest = hashlib.sha256(_read_plain_file(staged)).hexdigest()
        before = _profile_entries(profile_dir)
        prefix = f"input-{index:02d}-"
        environment = os.environ.copy()
        environment["LLVM_PROFILE_FILE"] = str(
            profile_dir / f"{prefix}%p-%m.profraw"
        )
        result = run_bounded(
            (str(binary), "js", "--pgo-training", str(staged)),
            environment=environment,
        )
        if result.timed_out:
            raise TrainingError(f"training input timed out: {case.path}")
        if result.output_exceeded:
            raise TrainingError(f"training output exceeded policy cap: {case.path}")
        if len(result.stdout) > STDOUT_LIMIT:
            raise TrainingError(f"training stdout exceeded policy cap: {case.path}")
        if result.returncode != 0:
            raise TrainingError(
                f"training input failed ({result.returncode}): {case.path}; "
                f"stderr={result.stderr[:512]!r}"
            )
        if result.stdout != case.stdout:
            raise TrainingError(
                f"training stdout mismatch: {case.path}; "
                f"expected={case.stdout!r}, actual={result.stdout!r}"
            )
        if result.stderr != case.stderr:
            raise TrainingError(
                f"training stderr mismatch: {case.path}; "
                f"expected={case.stderr!r}, actual={result.stderr!r}"
            )
        if hashlib.sha256(_read_plain_file(staged)).hexdigest() != before_digest:
            raise TrainingError(f"staged training input changed while running: {case.path}")
        after = _profile_entries(profile_dir)
        new_names = sorted(set(after) - set(before))
        if len(new_names) != 1 or not new_names[0].startswith(prefix):
            raise TrainingError(
                f"expected exactly one new profraw for {case.path}, got {new_names!r}"
            )
        profile_files.append(after[new_names[0]])
        print(
            f"trained {case.path}: stdout_sha256="
            f"{hashlib.sha256(result.stdout).hexdigest()} "
            f"stderr_sha256={hashlib.sha256(result.stderr).hexdigest()} "
            f"profile={new_names[0]}"
        )

    final_entries = _profile_entries(profile_dir)
    if set(final_entries.values()) != set(profile_files):
        raise TrainingError("profile directory contains an unenumerated file")
    check_plain_path(profile_list.parent, kind="directory")
    descriptor = os.open(
        profile_list,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0),
        0o400,
    )
    try:
        for path in profile_files:
            record = (
                str(path.resolve(strict=True)).encode("utf-8")
                + b"\0"
                + _sha256_path(path).encode("ascii")
                + b"\0"
            )
            view = memoryview(record)
            while view:
                written = os.write(descriptor, view)
                view = view[written:]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    os.chmod(profile_list, stat.S_IREAD)


def verify_profile_list(*, profile_dir: Path, profile_list: Path) -> tuple[Path, ...]:
    """Verify the exact profraw set and its captured hashes before/after merge."""

    directory = profile_dir.resolve(strict=True)
    check_plain_path(directory, kind="directory")
    raw = _read_plain_file(profile_list)
    fields = raw.split(b"\0")
    if not fields or fields[-1] != b"" or len(fields[:-1]) % 2:
        raise TrainingError("invalid profile-list record framing")
    fields = fields[:-1]
    paths: list[Path] = []
    seen: set[Path] = set()
    for index in range(0, len(fields), 2):
        try:
            path = Path(fields[index].decode("utf-8")).resolve(strict=True)
            expected = fields[index + 1].decode("ascii")
        except (UnicodeDecodeError, OSError) as exc:
            raise TrainingError("invalid profile-list record") from exc
        if SHA256_RE.fullmatch(expected) is None:
            raise TrainingError("invalid profile-list SHA-256")
        try:
            path.relative_to(directory)
        except ValueError as exc:
            raise TrainingError(f"profile-list path escapes profile directory: {path}") from exc
        if path in seen:
            raise TrainingError(f"duplicate profile-list path: {path}")
        seen.add(path)
        if _sha256_path(path) != expected:
            raise TrainingError(f"profile changed after training: {path.name}")
        paths.append(path)
    if not paths:
        raise TrainingError("profile list is empty")
    actual = set(_profile_entries(directory).values())
    if actual != seen:
        raise TrainingError("profile directory differs from the enumerated profile list")
    return tuple(paths)


def publish_atomic(
    *, source: Path, destination: Path, readonly: bool, reuse_identical: bool
) -> None:
    """Copy through an exclusive sibling and atomically replace one plain file."""

    content = _read_plain_file(source)
    digest = hashlib.sha256(content).hexdigest()
    parent, parent_info = _plain_lexical_directory(destination.parent)
    destination = parent / destination.name
    if destination.exists() or destination.is_symlink():
        _lstat_plain(destination, kind="file")
        if reuse_identical:
            if _sha256_path(destination) == digest:
                return
            raise TrainingError(
                f"digest-named publication has unexpected bytes: {destination}"
            )

    _require_identity(parent, parent_info, kind="directory")
    descriptor, raw_temporary = tempfile.mkstemp(prefix=".zipp-pgo-", dir=parent)
    temporary = Path(raw_temporary)
    temporary_info = os.fstat(descriptor)
    try:
        _require_identity(parent, parent_info, kind="directory")
        if not stat.S_ISREG(temporary_info.st_mode):
            raise TrainingError(f"publication temporary is not regular: {temporary}")
        view = memoryview(content)
        while view:
            written = os.write(descriptor, view)
            view = view[written:]
        os.fsync(descriptor)
        os.close(descriptor)
        descriptor = -1
        _require_identity(temporary, temporary_info, kind="file")
        if _sha256_path(temporary) != digest:
            raise TrainingError("publication temporary failed digest verification")
        if readonly:
            os.chmod(temporary, stat.S_IREAD)
        if destination.exists() or destination.is_symlink():
            _lstat_plain(destination, kind="file")
        _require_identity(parent, parent_info, kind="directory")
        os.replace(temporary, destination)
        _lstat_plain(destination, kind="file")
        if _sha256_path(destination) != digest:
            raise TrainingError("published file failed digest verification")
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        try:
            _require_identity(parent, parent_info, kind="directory")
            _require_identity(temporary, temporary_info, kind="file")
            temporary.unlink()
        except (FileNotFoundError, TrainingError):
            pass


def remove_plain_tree(path: Path) -> None:
    """Remove an owned temporary tree only after validating every entry."""

    root, root_info = _plain_lexical_directory(path)
    directories: list[tuple[Path, os.stat_result]] = [(root, root_info)]
    files: list[tuple[Path, os.stat_result]] = []
    for current, names, filenames in os.walk(root, topdown=True, followlinks=False):
        current_path = Path(current)
        for name in names:
            child = current_path / name
            directories.append((child, _lstat_plain(child, kind="directory")))
        for name in filenames:
            child = current_path / name
            files.append((child, _lstat_plain(child, kind="file")))
    directory_info = {directory: info for directory, info in directories}
    for directory, info in directories:
        _require_identity(root, root_info, kind="directory")
        _require_identity(directory, info, kind="directory")
        os.chmod(directory, stat.S_IWRITE | stat.S_IREAD | stat.S_IEXEC)
    for file_path, info in files:
        _require_identity(root, root_info, kind="directory")
        _require_identity(file_path.parent, directory_info[file_path.parent], kind="directory")
        _require_identity(file_path, info, kind="file")
        os.chmod(file_path, stat.S_IWRITE | stat.S_IREAD)
        file_path.unlink()
    for directory, info in sorted(
        directories, key=lambda item: len(item[0].parts), reverse=True
    ):
        _require_identity(root, root_info, kind="directory")
        _require_identity(directory, info, kind="directory")
        directory.rmdir()


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    stage = subparsers.add_parser("stage")
    stage.add_argument("--root", required=True, type=Path)
    stage.add_argument("--destination", required=True, type=Path)
    stage.add_argument("--file", action="append", required=True)
    run = subparsers.add_parser("run")
    run.add_argument("--root", required=True, type=Path)
    run.add_argument("--binary", required=True, type=Path)
    run.add_argument("--manifest", required=True, type=Path)
    run.add_argument("--profile-dir", required=True, type=Path)
    run.add_argument("--profile-list", required=True, type=Path)
    run.add_argument("--input", action="append", required=True)
    check = subparsers.add_parser("check")
    check.add_argument("--path", required=True, type=Path)
    check.add_argument("--kind", required=True, choices=("file", "directory"))
    check.add_argument("--empty", action="store_true")
    verify = subparsers.add_parser("verify-profiles")
    verify.add_argument("--profile-dir", required=True, type=Path)
    verify.add_argument("--profile-list", required=True, type=Path)
    publish = subparsers.add_parser("publish")
    publish.add_argument("--source", required=True, type=Path)
    publish.add_argument("--destination", required=True, type=Path)
    publish.add_argument("--readonly", action="store_true")
    publish.add_argument("--reuse-identical", action="store_true")
    remove = subparsers.add_parser("remove-tree")
    remove.add_argument("--path", required=True, type=Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.command == "stage":
            stage_files(root=args.root, destination=args.destination, paths=args.file)
        elif args.command == "run":
            run_training(
                root=args.root,
                binary=args.binary,
                manifest=args.manifest,
                profile_dir=args.profile_dir,
                profile_list=args.profile_list,
                inputs=args.input,
            )
        elif args.command == "check":
            check_plain_path(args.path, kind=args.kind, empty=args.empty)
        elif args.command == "verify-profiles":
            for path in verify_profile_list(
                profile_dir=args.profile_dir, profile_list=args.profile_list
            ):
                sys.stdout.buffer.write(str(path).encode("utf-8") + b"\0")
        elif args.command == "publish":
            publish_atomic(
                source=args.source,
                destination=args.destination,
                readonly=args.readonly,
                reuse_identical=args.reuse_identical,
            )
        else:
            remove_plain_tree(args.path)
    except (OSError, TrainingError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
