import { ChatScreen } from "./screens/ChatScreen";
import { ConnectScreen } from "./screens/ConnectScreen";
import { useConnected, useStartOnce } from "./state/store";

export function App() {
  useStartOnce();
  const connected = useConnected();
  return connected ? <ChatScreen /> : <ConnectScreen />;
}
