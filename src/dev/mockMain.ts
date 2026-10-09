// Entry of dev/mock.html: the real UI on top of the stand-in backend.

import { installMockBackend, mockState } from "./mockBackend";

installMockBackend();
// For UI tests: what the editor last sent, and the stand-in's other state.
(window as unknown as { __mock: typeof mockState }).__mock = mockState;
await import("../main");
