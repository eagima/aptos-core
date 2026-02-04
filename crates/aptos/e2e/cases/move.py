# Copyright © Aptos Foundation
# SPDX-License-Identifier: Apache-2.0

import json

from common import TestError
from test_helpers import RunHelper
from test_results import test_case


@test_case
def test_move_publish(run_helper: RunHelper, test_name=None):
    # Prior to this function running the move/ directory was moved into the working
    # directory in the host, which is then mounted into the container. The CLI is
    # then run in this directory, meaning the move/ directory is in the same directory
    # as the CLI is run from. This is why we can just refer to the package dir starting
    # with move/ here.
    package_dir = f"move/cli-e2e-tests/{run_helper.base_network}"

    # Publish the module.
    run_helper.run_command(
        test_name,
        [
            "aptos",
            "move",
            "publish",
            "--assume-yes",
            "--package-dir",
            package_dir,
            "--named-addresses",
            f"addr={str(run_helper.get_account_info().account_address)}",
        ],
    )

    # Get what modules exist on chain.
    response = run_helper.run_command(
        test_name,
        [
            "aptos",
            "account",
            "list",
            "--account",
            str(run_helper.get_account_info().account_address),
            "--query",
            "modules",
        ],
    )

    # Confirm that the module exists on chain.
    response = json.loads(response.stdout)
    for module in response["Result"]:
        if (
            module["abi"]["address"]
            == str(run_helper.get_account_info().account_address)
            and module["abi"]["name"] == "cli_e2e_tests"
        ):
            return

    raise TestError(
        "Module apparently published successfully but it could not be found on chain"
    )


@test_case
def test_move_compile(run_helper: RunHelper, test_name=None):
    package_dir = f"move/cli-e2e-tests/{run_helper.base_network}"
    account_info = run_helper.get_account_info()

    # Compile the module.
    response = run_helper.run_command(
        test_name,
        [
            "aptos",
            "move",
            "compile",
            "--package-dir",
            package_dir,
            "--named-addresses",
            f"addr={str(account_info.account_address)}",
        ],
    )

    if f"{str(account_info.account_address)[2:]}::cli_e2e_tests" not in response.stdout:
        raise TestError("Module did not compile successfully")


@test_case
def test_move_compile_dev_mode(run_helper: RunHelper, test_name=None):
    package_dir = f"move/cli-e2e-tests/{run_helper.base_network}"
    account_info = run_helper.get_account_info()

    # Compile the module.  Should not need an address passed in
    response = run_helper.run_command(
        test_name,
        [
            "aptos",
            "move",
            "compile",
            "--dev",
            "--package-dir",
            package_dir,
        ],
    )

    if f"{account_info.account_address}::cli_e2e_tests" not in response.stdout:
        raise TestError("Module did not compile successfully")


@test_case
def test_move_compile_fetch_deps_only(run_helper: RunHelper, test_name=None):
    package_dir = f"move/cli-e2e-tests/{run_helper.base_network}"
    account_info = run_helper.get_account_info()

    # Compile the module. Compilation should not be invoked, and return should be [].
    response = run_helper.run_command(
        test_name,
        [
            "aptos",
            "move",
            "compile",
            "--package-dir",
            package_dir,
            "--fetch-deps-only"
        ],
    )

    if f"{account_info.account_address}::cli_e2e_tests" in response.stdout:
        raise TestError("Module compilation should not be invoked")


@test_case
def test_move_compile_script(run_helper: RunHelper, test_name=None):
    package_dir = f"move/cli-e2e-tests/{run_helper.base_network}"
    account_info = run_helper.get_account_info()

    # Compile the script.
    response = run_helper.run_command(
        test_name,
        [
            "aptos",
            "move",
            "compile-script",
            "--package-dir",
            package_dir,
            "--named-addresses",
            f"addr={account_info.account_address}",
        ],
    )

    if "script_hash" not in response.stdout:
        raise TestError("Script did not compile successfully")


@test_case
def test_move_run(run_helper: RunHelper, test_name=None):
    account_info = run_helper.get_account_info()

    # Run the min_hero entry function with default profile
    response = run_helper.run_command(
        test_name,
        [
            "aptos",
            "move",
            "run",
            "--assume-yes",
            "--function-id",
            "default::cli_e2e_tests::mint_hero",
            "--args",
            "string:Boss",
            "string:Male",
            "string:Jin",
            "string:Undead",
            "string:",
        ],
    )

    if '"success": true' not in response.stdout:
        raise TestError("Move run did not execute successfully")

    # Get what modules exist on chain.
    response = run_helper.run_command(
        test_name,
        [
            "aptos",
            "move",
            "view",
            "--assume-yes",
            "--function-id",
            f"{str(account_info.account_address)}::cli_e2e_tests::view_hero",
            "--args",
            f"address:{str(account_info.account_address)}",
            "string:Hero Quest",
            "string:Jin",
        ],
    )

    result = json.loads(response.stdout)["Result"]
    if result[0].get("gender") != "Male" and result[0].get("race") != "Undead":
        raise TestError(
            "Data on chain (view_hero) does not match expected data from (mint_hero)"
        )

    # Run test_move_run to entry function with default profile
    # Make sure other parameters are able to be called using "move run"
    # Notice the entry function is not running anything but just testing the parameters
    response = run_helper.run_command(
        test_name,
        [
            "aptos",
            "move",
            "run",
            "--assume-yes",
            "--function-id",
            "default::cli_e2e_tests::test_move_run",
            "--args",
            "string:1234",  # Notice this is testing u8 vector instead of actual string
            "u16:[1,2]",
            "u32:[1,2]",
            "u64:[1,2]",
            "u128:[1,2]",
            "u256:[1,2]",
            'address:["0x123","0x456"]',
            "bool:[true,false]",
            'string:["abc","efg"]',
        ],
    )

    if '"success": true' not in response.stdout:
        raise TestError("Move run did not execute successfully")


@test_case
def test_move_view(run_helper: RunHelper, test_name=None):
    account_info = run_helper.get_account_info()

    # Run the view function
    response = run_helper.run_command(
        test_name,
        [
            "aptos",
            "move",
            "view",
            "--function-id",
            "0x1::account::exists_at",
            "--args",
            f"address:{account_info.account_address}",
        ],
    )

    response = json.loads(response.stdout)
    if response["Result"] == None or response["Result"][0] != True:
        raise TestError("View function did not return correct result")

    # Test view function with with big number arguments
    expected_u64 = 18446744073709551615
    expected_128 = 340282366920938463463374607431768211455
    expected_256 = (
        115792089237316195423570985008687907853269984665640564039457584007913129639935
    )
    response = run_helper.run_command(
        test_name,
        [
            "aptos",
            "move",
            "view",
            "--assume-yes",
            "--function-id",
            "default::cli_e2e_tests::test_big_number",
            "--args",
            f"u64:{expected_u64}",
            f"u128:{expected_128}",
            f"u256:{expected_256}",  # Important to test this big number
        ],
    )

    response = json.loads(response.stdout)
    if (
        response["Result"] == None
        or response["Result"][0] != f"{expected_u64}"
        or response["Result"][1] != f"{expected_128}"
        or response["Result"][2] != f"{expected_256}"
    ):
        raise TestError(
            f"View function [test_big_number] did not return correct result"
        )

    # Test view function with vector arguments
    # Follow 2 lines are for testing vector of u16-u256
    response = run_helper.run_command(
        test_name,
        [
            "aptos",
            "move",
            "view",
            "--assume-yes",
            "--function-id",
            "default::cli_e2e_tests::test_vector",
            "--args",
            "string:1234",  # Notice this is testing u8 vector instead of actual string
            f"u16:[1,2]",
            f"u32:[1,2]",
            f"u64:[1,2]",
            f"u128:[1,2]",
            f"u256:[1,2]",
            f'address:["0x123","0x456"]',
            "bool:[true,false]",
            'string:["abc","efg"]',
        ],
    )

    response = json.loads(response.stdout)
    if response["Result"] == None or len(response["Result"]) != 9:
        raise TestError(f"View function [test_vector] did not return correct result")


@test_case
def test_struct_argument_simple(run_helper: RunHelper, test_name=None):
    """Test passing a simple struct (Point) as transaction argument."""
    account_address = str(run_helper.get_account_info().account_address)

    # Create JSON file with struct argument
    import tempfile
    import os
    fd, json_path = tempfile.mkstemp(suffix=".json", text=True)
    try:
        json_content = {
            "function_id": "default::cli_e2e_tests::test_struct_point",
            "type_args": [],
            "args": [
                {
                    "type": f"{account_address}::cli_e2e_tests::Point",
                    "value": {
                        "x": "10",
                        "y": "20"
                    }
                }
            ]
        }
        with os.fdopen(fd, 'w') as f:
            json.dump(json_content, f)

        # Execute transaction with struct argument
        response = run_helper.run_command(
            test_name,
            [
                "aptos",
                "move",
                "run",
                "--assume-yes",
                "--json-file",
                json_path,
            ],
        )

        if '"success": true' not in response.stdout:
            raise TestError("Transaction with simple struct argument failed")
    finally:
        os.unlink(json_path)


@test_case
def test_struct_argument_nested(run_helper: RunHelper, test_name=None):
    """Test passing a nested struct (Rectangle with Points) as argument."""
    account_address = str(run_helper.get_account_info().account_address)

    import tempfile
    import os
    fd, json_path = tempfile.mkstemp(suffix=".json", text=True)
    try:
        json_content = {
            "function_id": "default::cli_e2e_tests::test_struct_rectangle",
            "type_args": [],
            "args": [
                {
                    "type": f"{account_address}::cli_e2e_tests::Rectangle",
                    "value": {
                        "top_left": {"x": "0", "y": "0"},
                        "bottom_right": {"x": "100", "y": "50"}
                    }
                }
            ]
        }
        with os.fdopen(fd, 'w') as f:
            json.dump(json_content, f)

        response = run_helper.run_command(
            test_name,
            [
                "aptos",
                "move",
                "run",
                "--assume-yes",
                "--json-file",
                json_path,
            ],
        )

        if '"success": true' not in response.stdout:
            raise TestError("Transaction with nested struct argument failed")
    finally:
        os.unlink(json_path)


@test_case
def test_option_variant_format(run_helper: RunHelper, test_name=None):
    """
    Test Option<T> with variant format: {"variant": "Some", "fields": [...]}
    """
    import tempfile
    import os

    # Test Option::Some
    fd_some, json_path_some = tempfile.mkstemp(suffix=".json", text=True)
    try:
        json_content = {
            "function_id": "default::cli_e2e_tests::test_option_some",
            "type_args": [],
            "args": [
                {
                    "type": "0x1::option::Option<u64>",
                    "value": {
                        "variant": "Some",
                        "fields": ["100"]
                    }
                }
            ]
        }
        with os.fdopen(fd_some, 'w') as f:
            json.dump(json_content, f)

        response = run_helper.run_command(
            test_name,
            [
                "aptos",
                "move",
                "run",
                "--assume-yes",
                "--json-file",
                json_path_some,
            ],
        )

        if '"success": true' not in response.stdout:
            raise TestError(
                "Transaction with Option::Some (variant format) failed"
            )
    finally:
        os.unlink(json_path_some)

    # Test Option::None
    fd_none, json_path_none = tempfile.mkstemp(suffix=".json", text=True)
    try:
        json_content = {
            "function_id": "default::cli_e2e_tests::test_option_none",
            "type_args": [],
            "args": [
                {
                    "type": "0x1::option::Option<u64>",
                    "value": {
                        "variant": "None",
                        "fields": []
                    }
                }
            ]
        }
        with os.fdopen(fd_none, 'w') as f:
            json.dump(json_content, f)

        response = run_helper.run_command(
            test_name,
            [
                "aptos",
                "move",
                "run",
                "--assume-yes",
                "--json-file",
                json_path_none,
            ],
        )

        if '"success": true' not in response.stdout:
            raise TestError(
                "Transaction with Option::None (variant format) failed"
            )
    finally:
        os.unlink(json_path_none)


@test_case
def test_option_vector_format(run_helper: RunHelper, test_name=None):
    """Test Option<T> with vector format: ["value"] for Some, [] for None"""
    import tempfile
    import os

    # Test Option::Some with vector format
    fd_some, json_path_some = tempfile.mkstemp(suffix=".json", text=True)
    try:
        json_content = {
            "function_id": "default::cli_e2e_tests::test_option_some",
            "type_args": [],
            "args": [
                {
                    "type": "0x1::option::Option<u64>",
                    "value": ["100"]
                }
            ]
        }
        with os.fdopen(fd_some, 'w') as f:
            json.dump(json_content, f)

        response = run_helper.run_command(
            test_name,
            [
                "aptos",
                "move",
                "run",
                "--assume-yes",
                "--json-file",
                json_path_some,
            ],
        )

        if '"success": true' not in response.stdout:
            raise TestError(
                "Transaction with Option::Some (vector format) failed"
            )
    finally:
        os.unlink(json_path_some)

    # Test Option::None with vector format
    fd_none, json_path_none = tempfile.mkstemp(suffix=".json", text=True)
    try:
        json_content = {
            "function_id": "default::cli_e2e_tests::test_option_none",
            "type_args": [],
            "args": [
                {
                    "type": "0x1::option::Option<u64>",
                    "value": []
                }
            ]
        }
        with os.fdopen(fd_none, 'w') as f:
            json.dump(json_content, f)

        response = run_helper.run_command(
            test_name,
            [
                "aptos",
                "move",
                "run",
                "--assume-yes",
                "--json-file",
                json_path_none,
            ],
        )

        if '"success": true' not in response.stdout:
            raise TestError(
                "Transaction with Option::None (vector format) failed"
            )
    finally:
        os.unlink(json_path_none)


@test_case
def test_option_with_struct(run_helper: RunHelper, test_name=None):
    """Test Option<Struct> - Option containing a struct type."""
    account_address = str(run_helper.get_account_info().account_address)
    import tempfile
    import os

    fd, json_path = tempfile.mkstemp(suffix=".json", text=True)
    try:
        json_content = {
            "function_id": "default::cli_e2e_tests::test_option_point",
            "type_args": [],
            "args": [
                {
                    "type": (
                        f"0x1::option::Option<{account_address}"
                        "::cli_e2e_tests::Point>"
                    ),
                    "value": {
                        "variant": "Some",
                        "fields": [
                            {
                                "x": "50",
                                "y": "75"
                            }
                        ]
                    }
                }
            ]
        }
        with os.fdopen(fd, 'w') as f:
            json.dump(json_content, f)

        response = run_helper.run_command(
            test_name,
            [
                "aptos",
                "move",
                "run",
                "--assume-yes",
                "--json-file",
                json_path,
            ],
        )

        if '"success": true' not in response.stdout:
            raise TestError("Transaction with Option<Struct> failed")
    finally:
        os.unlink(json_path)


@test_case
def test_mixed_primitive_and_struct_args(
    run_helper: RunHelper, test_name=None
):
    """Test mixing primitive type arguments with struct arguments."""
    account_address = str(run_helper.get_account_info().account_address)
    import tempfile
    import os

    fd, json_path = tempfile.mkstemp(suffix=".json", text=True)
    try:
        json_content = {
            "function_id": "default::cli_e2e_tests::test_mixed_args",
            "type_args": [],
            "args": [
                {"type": "u64", "value": "42"},
                {
                    "type": f"{account_address}::cli_e2e_tests::Point",
                    "value": {"x": "10", "y": "20"}
                },
                {"type": "bool", "value": True}
            ]
        }
        with os.fdopen(fd, 'w') as f:
            json.dump(json_content, f)

        response = run_helper.run_command(
            test_name,
            [
                "aptos",
                "move",
                "run",
                "--assume-yes",
                "--json-file",
                json_path,
            ],
        )

        if '"success": true' not in response.stdout:
            raise TestError("Transaction with mixed arguments failed")
    finally:
        os.unlink(json_path)


@test_case
def test_type_args_with_struct_args(run_helper: RunHelper, test_name=None):
    """Test that type_args work correctly with struct arguments."""
    account_address = str(run_helper.get_account_info().account_address)
    import tempfile
    import os

    fd, json_path = tempfile.mkstemp(suffix=".json", text=True)
    try:
        json_content = {
            "function_id": "default::cli_e2e_tests::test_generic_with_struct",
            "type_args": ["u64", "address"],
            "args": [
                {
                    "type": f"{account_address}::cli_e2e_tests::Point",
                    "value": {"x": "15", "y": "25"}
                },
                {"type": "u64", "value": "999"}
            ]
        }
        with os.fdopen(fd, 'w') as f:
            json.dump(json_content, f)

        response = run_helper.run_command(
            test_name,
            [
                "aptos",
                "move",
                "run",
                "--assume-yes",
                "--json-file",
                json_path,
            ],
        )

        if '"success": true' not in response.stdout:
            raise TestError(
                "Transaction with type_args and struct arguments failed"
            )
    finally:
        os.unlink(json_path)


@test_case
def test_struct_with_vector_field(run_helper: RunHelper, test_name=None):
    """Test struct containing a vector field."""
    account_address = str(run_helper.get_account_info().account_address)
    import tempfile
    import os

    fd, json_path = tempfile.mkstemp(suffix=".json", text=True)
    try:
        json_content = {
            "function_id": "default::cli_e2e_tests::test_data_struct",
            "type_args": [],
            "args": [
                {
                    "type": f"{account_address}::cli_e2e_tests::Data",
                    "value": {
                        "values": ["1", "2", "3", "4", "5"],
                        "name": "test_data"
                    }
                }
            ]
        }
        with os.fdopen(fd, 'w') as f:
            json.dump(json_content, f)

        response = run_helper.run_command(
            test_name,
            [
                "aptos",
                "move",
                "run",
                "--assume-yes",
                "--json-file",
                json_path,
            ],
        )

        if '"success": true' not in response.stdout:
            raise TestError(
                "Transaction with struct containing vector field failed"
            )
    finally:
        os.unlink(json_path)
