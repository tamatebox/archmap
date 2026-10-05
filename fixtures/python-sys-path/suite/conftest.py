import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT / "tasks") not in sys.path:
    sys.path.insert(0, str(ROOT / "tasks"))
sys.path.append(os.path.join(os.path.dirname(__file__), "..", "src", "vendor"))
sys.path.insert(0, os.environ.get("EXTRA_PATH", ""))
