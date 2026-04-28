"""HTTP client for the ACC fleet API.

Public API
----------
.. list-table::
   :header-rows: 1

   * - Name
     - Description
   * - :class:`Client`
     - Synchronous client; the common case
   * - :class:`ApiError`
     - HTTP 4xx/5xx wrapped in a typed exception
   * - :class:`Conflict`
     - HTTP 409 (claim race etc.)
   * - :class:`Locked`
     - HTTP 423 (task blocked by dependencies)
   * - :class:`NotFound`
     - HTTP 404
   * - :class:`Unauthorized`
     - HTTP 401
   * - :class:`AtCapacity`
     - HTTP 429
   * - :class:`NoToken`
     - No API token found in env / dotenv

Sub-API classes (available as ``client.<name>``)
------------------------------------------------
These mirror the corresponding modules in the Rust ``acc-client`` crate:

.. list-table::
   :header-rows: 1

   * - Attribute
     - Class
     - Rust analogue
   * - ``client.tasks``
     - :class:`~acc_client._tasks.TasksApi`
     - ``acc_client::tasks``
   * - ``client.projects``
     - :class:`~acc_client._projects.ProjectsApi`
     - ``acc_client::projects``
   * - ``client.queue``
     - :class:`~acc_client._queue.QueueApi`
     - ``acc_client::queue``
   * - ``client.items``
     - :class:`~acc_client._items.ItemsApi`
     - ``acc_client::items``
   * - ``client.bus``
     - :class:`~acc_client._bus.BusApi`
     - ``acc_client::bus``
   * - ``client.memory``
     - :class:`~acc_client._memory.MemoryApi`
     - ``acc_client::memory``
   * - ``client.agents``
     - :class:`~acc_client._agents.AgentsApi`
     - ``acc_client::agents``
"""

from ._agents import AgentsApi
from ._bus import BusApi
from ._errors import (
    ApiError,
    AtCapacity,
    Conflict,
    Locked,
    NotFound,
    NoToken,
    Unauthorized,
)
from ._items import ItemsApi
from ._memory import MemoryApi
from ._projects import ProjectsApi
from ._queue import QueueApi
from ._tasks import TasksApi
from .client import Client

__all__ = [
    # Primary client
    "Client",
    # Exceptions
    "ApiError",
    "Conflict",
    "Locked",
    "NotFound",
    "Unauthorized",
    "AtCapacity",
    "NoToken",
    # Sub-API classes (exported so callers can type-hint parameters)
    "TasksApi",
    "ProjectsApi",
    "QueueApi",
    "ItemsApi",
    "BusApi",
    "MemoryApi",
    "AgentsApi",
]

__version__ = "0.1.0"
