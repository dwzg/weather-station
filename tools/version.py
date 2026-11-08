# Injects the firmware version (git describe) as FW_VERSION.
import subprocess

Import("env")  # noqa: F821  (provided by PlatformIO)

try:
    version = subprocess.check_output(
        ["git", "describe", "--tags", "--always", "--dirty"], text=True, stderr=subprocess.DEVNULL
    ).strip()
except Exception:
    version = "unknown"
env.Append(CPPDEFINES=[("FW_VERSION", env.StringifyMacro(version))])  # noqa: F821
