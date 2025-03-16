import * as lmq from "lmq";

const WEBSOCKET_PORT = 8367;

// Initialize lmq.
await lmq.default();

// Global state.
const GLOBAL_LINK_MESSAGE_QUEUE = new lmq.LinkMessageQueue();
let webSocket: WebSocket | null = null;
let isConnectedInternal = false;
let connectionListeners: Array<(connected: boolean) => void> = [];
const reconnectInterval = 1000; // ms

/**
 * Initialize
 * 
 * @returns Promise that resolves when connection is established
 */
export function init(): Promise<void> {
  return new Promise((resolve, reject) => {
    if (!webSocket) {
      // Create WebSocket and set up automatic reconnection.
      connectWebSocket();
    }

    // Just in case if connection in progress, add listeners first.
    webSocket!.addEventListener("open", () => {
      resolve();
    }, { once: true });
    // Watch for errors.
    webSocket!.addEventListener("error", () => {
      // In case we've connected but an error occurs afterwards.
      if (!isConnectedInternal) {
        reject(new Error("Disconnected from SkyCurrent WebSocket gateway"));
      }
    }, { once: true });
    // Watch for close.
    webSocket!.addEventListener("close", () => {
      reject(new Error("Disconnected from SkyCurrent WebSocket gateway"));
    }, { once: true });

    // Check connection state using readyState.
    if (webSocket!.readyState === WebSocket.OPEN) {
      // Already connected.
      resolve();
    } else if (webSocket?.readyState === WebSocket.CLOSING || webSocket?.readyState === WebSocket.CLOSED) {
      // CLOSING or CLOSED state - create a new connection.
      connectWebSocket();
    }/* else if (webSocket.readyState === WebSocket.CONNECTING) {  }*/
  });
}

/**
 * Internal function to establish and maintain WebSocket connection
 */
function connectWebSocket() {
  try {
    webSocket = new WebSocket(`ws://localhost:${WEBSOCKET_PORT}`);

    webSocket.binaryType = "arraybuffer";

    webSocket.addEventListener("open", () => {
      isConnectedInternal = true;
      notifyConnectionListeners();
      console.log("Connected to gateway!");
    });

    webSocket.addEventListener("close", () => {
      isConnectedInternal = false;
      notifyConnectionListeners();
      console.log(`Disconnected from SkyCurrent WebSocket gateway server, will attempt to reconnect in ${reconnectInterval}ms...`);

      // Try to reconnect after interval.
      setTimeout(init, reconnectInterval);
    });

    webSocket.addEventListener("message", (event) => {
      if (event.data instanceof ArrayBuffer) {
        const buffer = new Uint8Array(event.data);

        // // Extract the header size (last 8 bytes, little-endian).
        // const headerSizeBuffer = buffer.slice(buffer.length - 8);
        // const headerSizeView = new DataView(headerSizeBuffer.buffer);
        // const headerSize = Number(headerSizeView.getBigUint64(0, true)); // true for little-endian.
        
        // Extract the actual data (excluding the 8-byte header size field).
        const actualData = buffer.slice(0, buffer.length - 8);

        // Add message to the global queue.
        GLOBAL_LINK_MESSAGE_QUEUE.push(actualData);
      }
    });
  } catch (error) {
    console.error("Error connecting to WebSocket:", error);
    isConnectedInternal = false;
    notifyConnectionListeners();

    // Try to reconnect after interval.
    setTimeout(init, reconnectInterval);
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
      if (webSocket && webSocket.readyState === WebSocket.OPEN) {
        webSocket.send(resultBuffer);  // Browser will only throw if we try to send when WebSocket is in CONNECTING state, which it isn't, so this is alright.
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
 * Send a message and prepare to receive replies
 * 
 * @param payload The data to send
 * @param headerSize Size of the header section in bytes
 * @returns Promise that resolves with a MessageConsumer for receiving potential replies once the message is sent
 */
export async function dlgStream(payload: Uint8Array | Array<number>, headerSize: number): Promise<lmq.MessageConsumer> {
  const consumer = iterStream();
  await sendStream(payload, headerSize);
  return consumer;
}

/**
 * Get an iterator over the message stream
 * 
 * @returns MessageConsumer for iterating through messages
 */
export function iterStream(): lmq.MessageConsumer {
  return GLOBAL_LINK_MESSAGE_QUEUE.create_consumer();
}

/**
 * Check if WebSocket connection is currently active
 * 
 * @returns Current connection status
 */
export function isConnected(): boolean {
  return isConnectedInternal;
}

/**
 * Add a listener for connection status changes
 * 
 * @param listener Function to call when connection status changes
 * @returns Function to remove the listener
 */
export function onConnectionChange(listener: (connected: boolean) => void): () => void {
  connectionListeners.push(listener);

  // Notify immediately of current state.
  listener(isConnectedInternal);

  // Return function to remove listener.
  return () => {
    connectionListeners = connectionListeners.filter((l) => l !== listener);
  };
}

/**
 * Notify all connection listeners of current status
 */
function notifyConnectionListeners() {
  for (const listener of connectionListeners) {
    try {
      listener(isConnectedInternal);
    } catch (error) {
      console.error("Error in connection listener:", error);
    }
  }
}

/**
 * Close the WebSocket connection
 */
export function close(): void {
  if (webSocket) {
    const socket = webSocket;
    webSocket = null;
    isConnectedInternal = false;
    notifyConnectionListeners();
    socket.close();
  }
}

