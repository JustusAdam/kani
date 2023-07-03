#!/usr/bin/env python3
from subprocess import run, TimeoutExpired
from pathlib import Path
import os
import argparse

def mvm(name: str, key_t: str = "u8", val_t: str = "u8") -> str:
    '''
    Monomorphized VecMap. This function qualifies the unqualified method `name`
    with `VecMap` and the key type `key_t` and value type `val_t`.
    '''
    return f"VecMap::<{key_t}, {val_t}>::{name}"

def ch(name: str) -> str:
    return "contract_harnesses::" + name

TEST_CASES = [
    (mvm("new"), ch("new_1")),
    (mvm("new", key_t = "u64", val_t = "std::option::Option<i32>"), ch("new_2")),
    (mvm("with_capacity"), ch("with_capacity")),
    (mvm("clear"), ch("clear")),
]

TEST_FILE = "fixme_vec_map_example.rs"

def run_tests(test_cases, args):
    timeout = args.timeout
    for (function, harness) in test_cases:
        print(f"Checking function contract for {function} on {harness}")
        try:
            ret = run(("kani", "--check-contract", f"{function}/{harness}", TEST_FILE), capture_output=True, timeout=timeout, text=True)
            if ret.returncode != 0:
                print(f"Exited with non-zero code {ret.returncode}")
                if args.fail_fast:
                    print("------------ stdout ------------")
                    print(ret.stdout)
                    print("------------ stderr ------------")
                    print(ret.stderr)
                    return
            else:
                print("Finished successfully")
        except TimeoutExpired:
            print(f"Timed out after {timeout}s")
            if args.fail_fast:
                return

def main():

    parser = argparse.ArgumentParser()
    parser.add_argument("--fail-fast", action="store_true")
    parser.add_argument("--index", type=int)
    parser.add_argument("--timeout", type=float)
    args = parser.parse_args()

    print("Building kani")

    mypath = Path(__file__)
    basedir = mypath.parent.parent.parent.parent
    scriptdir = basedir / "scripts"

    run(["cargo", "build-dev"], cwd=basedir)

    oldpath = os.environ["PATH"]

    if not str(scriptdir) in oldpath:
        os.environ["PATH"] = f"{scriptdir}:{oldpath}"

    test_cases = TEST_CASES if args.index is None else [TEST_CASES[args.index]]

    run_tests(test_cases, args)

if __name__ == '__main__':
    main()