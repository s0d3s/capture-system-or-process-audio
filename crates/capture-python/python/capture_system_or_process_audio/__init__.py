from . import capture_system_or_process_audio as capture_system_or_process_audio  # type: ignore[attr-defined]
from .capture_system_or_process_audio import *  # type: ignore[import-not-found]

__doc__ = capture_system_or_process_audio.__doc__
if hasattr(capture_system_or_process_audio, "__all__"):
    __all__ = capture_system_or_process_audio.__all__

