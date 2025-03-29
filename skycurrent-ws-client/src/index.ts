import * as lmq from "lmq-web";

const WEBSOCKET_PORT = 8367;

// Define types for global state.
interface SkyCurrentStreamGlobals {
  queue: lmq.LinkMessageQueue;
  webSocket: WebSocket | null;
  isConnectedInternal: boolean;
  connectionListeners: Array<(connected: boolean) => void>;
  reconnectInterval: number;
}

// Ensure the global namespace exists.
(globalThis as any).__skycurrentstreamglobals = (globalThis as any).__skycurrentstreamglobals || {};
const globals = (globalThis as any).__skycurrentstreamglobals as SkyCurrentStreamGlobals;

// Initialize globals if they don't exist.
// This is why we can omit ? from the fields of the interface definition above.
if (globals.webSocket === undefined) {
  globals.webSocket = null;
}
if (globals.isConnectedInternal === undefined) {
  globals.isConnectedInternal = false;
}
if (globals.connectionListeners === undefined) {
  globals.connectionListeners = [];
}
if (globals.reconnectInterval === undefined) {
  globals.reconnectInterval = 1000;  //ms
}

/**
 * Initialize
 * 
 * @returns Promise that resolves when connection is established
 */
export function init(): Promise<void> {
  // Initialize lmq.
  return lmq.default().catch(console.error).then(_wasm => {
    return new Promise((resolve, reject) => {
      // Initialize global queue if it doesn't exist.
      if (globals.queue === undefined) {
        globals.queue = new lmq.LinkMessageQueue();
      }
  
      if (!globals.webSocket) {
        // Create WebSocket and set up automatic reconnection.
        connectWebSocket();
      }
  
      // Just in case if connection in progress, add listeners first.
      globals.webSocket!.addEventListener("open", () => {
        resolve();
      }, { once: true });
      // Watch for errors.
      globals.webSocket!.addEventListener("error", () => {
        // In case we've connected but an error occurs afterwards.
        if (!globals.isConnectedInternal) {
          reject(new Error("Disconnected from SkyCurrent WebSocket gateway"));
        }
      }, { once: true });
      // Watch for close.
      globals.webSocket!.addEventListener("close", () => {
        reject(new Error("Disconnected from SkyCurrent WebSocket gateway"));
      }, { once: true });
  
      // Check connection state using readyState.
      if (globals.webSocket!.readyState === WebSocket.OPEN) {
        // Already connected.
        resolve();
      } else if (globals.webSocket!.readyState === WebSocket.CLOSING || globals.webSocket!.readyState === WebSocket.CLOSED) {
        // CLOSING or CLOSED state - create a new connection.
        connectWebSocket();
      }/* else if (webSocket.readyState === WebSocket.CONNECTING) {  }*/
    });
  });
}

/**
 * Internal function to establish and maintain WebSocket connection
 */
function connectWebSocket() {
  try {
    globals.webSocket = new WebSocket(`ws://localhost:${WEBSOCKET_PORT}`);

    globals.webSocket.binaryType = "arraybuffer";

    globals.webSocket.addEventListener("open", () => {
      globals.isConnectedInternal = true;
      notifyConnectionListeners();
      console.log("Connected to gateway!");
    });

    globals.webSocket.addEventListener("close", () => {
      globals.isConnectedInternal = false;
      notifyConnectionListeners();
      console.log(`Disconnected from SkyCurrent WebSocket gateway server, will attempt to reconnect in ${globals.reconnectInterval}ms...`);

      // Try to reconnect after interval.
      setTimeout(init, globals.reconnectInterval);
    });

    globals.webSocket.addEventListener("message", (event) => {
      if (event.data instanceof ArrayBuffer) {
        const buffer = new Uint8Array(event.data);

        // // Extract the header size (last 8 bytes, little-endian).
        // const headerSizeBuffer = buffer.slice(buffer.length - 8);
        // const headerSizeView = new DataView(headerSizeBuffer.buffer);
        // const headerSize = Number(headerSizeView.getBigUint64(0, true)); // true for little-endian.
        
        // Extract the actual data (excluding the 8-byte header size field).
        const actualData = buffer.slice(0, buffer.length - 8);

        // Add message to the global queue.
        globals.queue.push(actualData);
      }
    });
  } catch (error) {
    console.error("Error connecting to WebSocket:", error);
    globals.isConnectedInternal = false;
    notifyConnectionListeners();

    // Try to reconnect after interval.
    setTimeout(init, globals.reconnectInterval);
  }
}

/**
 * Send an arbitrarily-sized payload
 * 
 * @param payload The data to send
 * @param headerSize Size of the header section in bytes
 * @returns Promise that resolves when message is sent
 */
export function sendStream(payload: Uint8Array | Array<number>, headerSize: number): Promise<void> {
  return new Promise((resolve, _) => {
    // Format the message according to wire format:
    // <actual binary data><8-bytes little-endian representing header size>
    const headerSizeBuffer = new ArrayBuffer(8);
    const headerSizeView = new DataView(headerSizeBuffer);
    headerSizeView.setBigUint64(0, BigInt(headerSize), true); // true for little-endian.

    // Combine payload with header size.
    const payloadArray = payload instanceof Uint8Array ? payload : new Uint8Array(payload);
    const resultBuffer = new Uint8Array(payloadArray.length + 8);
    resultBuffer.set(payloadArray, 0);
    resultBuffer.set(new Uint8Array(headerSizeBuffer), payloadArray.length);

    // Function to attempt sending.
    const attemptSend = () => {
      if (globals.webSocket && globals.webSocket.readyState === WebSocket.OPEN) {
        globals.webSocket.send(resultBuffer);  // Browser will only throw if we try to send when WebSocket is in CONNECTING state, which it isn't, so this is alright.
        resolve();
      } else {
        // Not connected, wait for connection.
        const removeListener = onConnectionChange((connected) => {
          if (connected) {
            // Connection established, try to send again.
            removeListener();  // Remove this listener to avoid memory leaks.
            attemptSend();
          }
        });
      }
    };

    // Start the sending process.
    attemptSend();
  });
}

/**
 * Get an iterator over the message stream
 * 
 * @returns MessageConsumer for iterating through messages
 */
export function iterStream(): lmq.MessageConsumer {
  return globals.queue.create_consumer();
}

/**
 * Check if WebSocket connection is currently active
 * 
 * @returns Current connection status
 */
export function isConnected(): boolean {
  return globals.isConnectedInternal;
}

/**
 * Add a listener for connection status changes
 * 
 * @param listener Function to call when connection status changes
 * @returns Function to remove the listener
 */
export function onConnectionChange(listener: (connected: boolean) => void): () => void {
  globals.connectionListeners.push(listener);

  // Notify immediately of current state.
  listener(globals.isConnectedInternal);

  // Return function to remove listener.
  return () => {
    globals.connectionListeners = globals.connectionListeners.filter((l) => l !== listener);
  };
}

/**
 * Notify all connection listeners of current status
 */
function notifyConnectionListeners() {
  for (const listener of globals.connectionListeners) {
    try {
      listener(globals.isConnectedInternal);
    } catch (error) {
      console.error("Error in connection listener:", error);
    }
  }
}

/**
 * Close the WebSocket connection
 */
export function close(): void {
  if (globals.webSocket) {
    const socket = globals.webSocket;
    globals.webSocket = null;
    globals.isConnectedInternal = false;
    notifyConnectionListeners();
    socket.close();
  }
}

