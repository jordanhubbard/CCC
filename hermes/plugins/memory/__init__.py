"""Memory provider plugin discovery.

Scans ``plugins/memory/<name>/`` directories for memory provider plugins.
Each subdirectory must contain ``__init__.py`` that either:

* exports a ``register(ctx)`` function that calls
  ``ctx.register_memory_provider(provider_instance)``, or
* defines a top-level class that extends
  :class:`agent.memory_provider.MemoryProvider`.

Usage (from ``run_agent.py``)::

    from plugins.memory import load_memory_provider

    provider = load_memory_provider("acc_shared_memory")
    if provider and provider.is_available():
        memory_manager.add_provider(provider)
"""

from __future__ import annotations

import importlib
import importlib.util
import logging
import sys
from pathlib import Path
from typing import List, Optional, Tuple

logger = logging.getLogger(__name__)

_MEMORY_PLUGINS_DIR = Path(__file__).parent


def discover_memory_providers() -> List[Tuple[str, str, bool]]:
    """Scan ``plugins/memory/`` for available memory providers.

    Returns a list of ``(name, description, is_available)`` tuples.
    Does NOT import the providers — just reads ``plugin.yaml`` for metadata
    and does a lightweight availability check.
    """
    results = []
    if not _MEMORY_PLUGINS_DIR.is_dir():
        return results

    for child in sorted(_MEMORY_PLUGINS_DIR.iterdir()):
        if not child.is_dir() or child.name.startswith(("_", ".")):
            continue
        init_file = child / "__init__.py"
        if not init_file.exists():
            continue

        desc = ""
        yaml_file = child / "plugin.yaml"
        if yaml_file.exists():
            try:
                import yaml  # type: ignore[import-untyped]

                with open(yaml_file) as f:
                    meta = yaml.safe_load(f) or {}
                desc = meta.get("description", "")
            except Exception:
                pass

        available = True
        try:
            provider = load_memory_provider(child.name)
            if provider is None:
                available = False
            elif hasattr(provider, "is_available"):
                available = provider.is_available()
        except Exception:
            available = False

        results.append((child.name, desc, available))

    return results


def load_memory_provider(name: str) -> "Optional[MemoryProvider]":
    """Load and return a :class:`~agent.memory_provider.MemoryProvider` by name.

    Returns ``None`` if the provider is not found or fails to load.
    """
    provider_dir = _MEMORY_PLUGINS_DIR / name
    if not provider_dir.is_dir():
        logger.debug(
            "Memory provider '%s' not found in %s", name, _MEMORY_PLUGINS_DIR
        )
        return None

    try:
        provider = _load_provider_from_dir(provider_dir)
        if provider:
            return provider
        logger.warning(
            "Memory provider '%s' loaded but no provider instance found", name
        )
        return None
    except Exception as e:
        logger.warning("Failed to load memory provider '%s': %s", name, e)
        return None


def _load_provider_from_dir(provider_dir: Path) -> "Optional[MemoryProvider]":
    """Import a provider module and extract the MemoryProvider instance."""
    name = provider_dir.name
    module_name = f"plugins.memory.{name}"
    init_file = provider_dir / "__init__.py"

    if not init_file.exists():
        return None

    if module_name in sys.modules:
        mod = sys.modules[module_name]
    else:
        # Ensure parent packages are registered before loading the child.
        for parent_pkg, parent_path in (
            ("plugins", Path(__file__).parent.parent),
            ("plugins.memory", _MEMORY_PLUGINS_DIR),
        ):
            if parent_pkg not in sys.modules:
                parent_init = parent_path / "__init__.py"
                if parent_init.exists():
                    spec = importlib.util.spec_from_file_location(
                        parent_pkg,
                        str(parent_init),
                        submodule_search_locations=[str(parent_path)],
                    )
                    if spec:
                        parent_mod = importlib.util.module_from_spec(spec)
                        sys.modules[parent_pkg] = parent_mod
                        try:
                            spec.loader.exec_module(parent_mod)  # type: ignore[union-attr]
                        except Exception:
                            pass

        spec = importlib.util.spec_from_file_location(
            module_name,
            str(init_file),
            submodule_search_locations=[str(provider_dir)],
        )
        if not spec:
            return None

        mod = importlib.util.module_from_spec(spec)
        sys.modules[module_name] = mod

        # Pre-register submodules so relative imports inside the plugin work.
        for sub_file in provider_dir.glob("*.py"):
            if sub_file.name == "__init__.py":
                continue
            full_sub = f"{module_name}.{sub_file.stem}"
            if full_sub not in sys.modules:
                sub_spec = importlib.util.spec_from_file_location(
                    full_sub, str(sub_file)
                )
                if sub_spec:
                    sub_mod = importlib.util.module_from_spec(sub_spec)
                    sys.modules[full_sub] = sub_mod
                    try:
                        sub_spec.loader.exec_module(sub_mod)  # type: ignore[union-attr]
                    except Exception as e:
                        logger.debug(
                            "Failed to pre-load submodule %s: %s", full_sub, e
                        )

        try:
            spec.loader.exec_module(mod)  # type: ignore[union-attr]
        except Exception as e:
            logger.debug("Failed to exec_module %s: %s", module_name, e)
            sys.modules.pop(module_name, None)
            return None

    # Pattern 1: register(ctx) — plugin-style registration.
    if hasattr(mod, "register"):
        collector = _ProviderCollector()
        try:
            mod.register(collector)
            if collector.provider:
                return collector.provider
        except Exception as e:
            logger.debug("register() failed for '%s': %s", name, e)

    # Pattern 2: top-level MemoryProvider subclass.
    try:
        from agent.memory_provider import MemoryProvider  # type: ignore[import]

        for attr_name in dir(mod):
            attr = getattr(mod, attr_name, None)
            if (
                isinstance(attr, type)
                and issubclass(attr, MemoryProvider)
                and attr is not MemoryProvider
            ):
                try:
                    return attr()
                except Exception:
                    pass
    except ImportError:
        pass

    return None


class _ProviderCollector:
    """Minimal plugin context that captures ``register_memory_provider`` calls."""

    def __init__(self) -> None:
        self.provider: "Optional[MemoryProvider]" = None

    def register_memory_provider(self, provider: "MemoryProvider") -> None:
        self.provider = provider

    # No-op stubs for other registration methods so plugins that call them
    # during registration don't raise AttributeError.
    def register_tool(self, *args: object, **kwargs: object) -> None:
        pass

    def register_hook(self, *args: object, **kwargs: object) -> None:
        pass

    def register_cli_command(self, *args: object, **kwargs: object) -> None:
        pass

    def register_context_engine(self, *args: object, **kwargs: object) -> None:
        pass
