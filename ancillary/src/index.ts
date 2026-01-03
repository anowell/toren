import { AncillaryRuntime } from './ancillary-runtime.js';
import { ShipClient } from './ship-client.js';
import { detectSegmentName } from './segment-naming.js';
import * as dotenv from 'dotenv';

dotenv.config();

async function main() {
  const shipUrl = process.env.SHIP_URL || 'ws://localhost:8787';
  const sessionToken = process.env.SESSION_TOKEN;
  const anthropicApiKey = process.env.ANTHROPIC_API_KEY;

  // Segment and ancillary naming (e.g., "Howie One", "Agent Two")
  const segmentName = process.env.SEGMENT_NAME || detectSegmentName();
  const ancillaryNumber = process.env.ANCILLARY_NUMBER || 'One';
  const ancillaryId = `${segmentName} ${ancillaryNumber}`;

  if (!anthropicApiKey) {
    console.error('ANTHROPIC_API_KEY environment variable is required');
    process.exit(1);
  }

  console.log(`Ancillary ${ancillaryId} initializing...`);
  console.log(`Connecting to ship: ${shipUrl}`);

  // Initialize ship client
  const shipClient = new ShipClient(shipUrl, sessionToken, ancillaryId);
  await shipClient.connect();

  // Initialize ancillary runtime
  const runtime = new AncillaryRuntime(anthropicApiKey, shipClient, ancillaryId);
  await runtime.start();

  console.log(`${ancillaryId} synchronized with Toren`);

  // Handle shutdown
  process.on('SIGINT', async () => {
    console.log(`\n${ancillaryId} disconnecting...`);
    await runtime.stop();
    await shipClient.disconnect();
    process.exit(0);
  });
}

main().catch((error) => {
  console.error('Fatal error:', error);
  process.exit(1);
});
