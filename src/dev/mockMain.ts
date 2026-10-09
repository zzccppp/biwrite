// Entry of dev/mock.html: the real UI on top of the stand-in backend.

import { installMockBackend } from "./mockBackend";

installMockBackend();
await import("../main");
