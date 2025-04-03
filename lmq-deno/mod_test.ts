import { assert, assertEquals } from "jsr:@std/assert";
import { LinkMessageQueue } from "./mod.ts";

Deno.test("Queue push and next", async () => {
  const queue = new LinkMessageQueue();
  const consumer = queue.create_consumer();
  const msgContent = new TextEncoder().encode("test message");
  queue.push(msgContent);
  const msgContent2 = new TextEncoder().encode("test message 2");
  queue.push(msgContent2);

  const message = await consumer.next();
  assert(message !== null);
  const readData = message.read();
  assert(readData);
  const text = new TextDecoder().decode(readData);
  assertEquals(text, "test message");
  message.free();

  const message2 = await consumer.next();
  assert(message2 !== null);
  const readData2 = message2.read();
  assert(readData2);
  const text2 = new TextDecoder().decode(readData2);
  assertEquals(text2, "test message 2");

  message2.free();
  consumer.free();
  queue.free();
});

Deno.test("Try next returns null when no message available", async () => {
  const queue = new LinkMessageQueue();
  const consumer = queue.create_consumer();

  const result = await consumer.try_next();
  assertEquals(result, null);

  consumer.free();
  queue.free();
});

Deno.test("Claim returns correct data and read then fails", async () => {
  const queue = new LinkMessageQueue();
  const consumer = queue.create_consumer();
  const msgContent = new TextEncoder().encode("claim test");
  queue.push(msgContent);

  const message = await consumer.next();
  const claimed = message.claim();
  assert(claimed !== undefined);
  const textClaimed = new TextDecoder().decode(claimed);
  assertEquals(textClaimed, "claim test");

  // After claiming, read should return undefined.
  assertEquals(message.read(), undefined);

  message.free();
  consumer.free();
  queue.free();
});

