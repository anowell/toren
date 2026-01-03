#!/usr/bin/env -S npx tsx

/**
 * Toren CLI - Direct Claude to build apps through the Toren daemon
 *
 * Usage:
 *   ./toren-cli.ts "Build a web server on port 3000"
 *   echo "Create a calculator" | ./toren-cli.ts
 *   ./toren-cli.ts < prompt.txt
 *   npx tsx toren-cli.ts "Build something"
 */

import { AncillaryRuntime } from '../ancillary/dist/ancillary-runtime.js';
import { ShipClient } from '../ancillary/dist/ship-client.js';
import * as fs from 'fs';
import * as dotenv from 'dotenv';

dotenv.config();

async function main() {
  const sessionToken = process.env.SESSION_TOKEN;
  const apiKey = process.env.ANTHROPIC_API_KEY;
  const shipUrl = process.env.SHIP_URL || 'ws://localhost:8787';
  const workingDir = process.env.WORKING_DIR || process.cwd();

  // Validate environment
  if (!sessionToken) {
    console.error('❌ Error: SESSION_TOKEN environment variable is required');
    console.error('\nGet a session token:');
    console.error('  1. Start daemon: ./target/release/toren-daemon');
    console.error('  2. Note the pairing token (e.g., 714697)');
    console.error('  3. curl -X POST http://localhost:8787/pair \\');
    console.error('       -H "Content-Type: application/json" \\');
    console.error('       -d \'{"pairing_token": "714697"}\'');
    console.error('  4. Export SESSION_TOKEN=<token-from-response>');
    process.exit(1);
  }

  if (!apiKey) {
    console.error('❌ Error: ANTHROPIC_API_KEY environment variable is required');
    process.exit(1);
  }

  // Derive ancillary name from working directory
  const dirName = workingDir.split('/').pop() || 'Unknown';
  const segmentName = dirName.charAt(0).toUpperCase() + dirName.slice(1);
  const ancillaryId = `${segmentName} One`;

  // Get prompt from args or stdin
  let prompt = '';

  if (process.argv.length > 2) {
    // From command line args
    prompt = process.argv.slice(2).join(' ');
  } else if (!process.stdin.isTTY) {
    // From stdin (piped or redirected)
    const chunks: Buffer[] = [];
    for await (const chunk of process.stdin) {
      chunks.push(chunk);
    }
    prompt = Buffer.concat(chunks).toString('utf-8').trim();
  } else {
    // Interactive mode - prompt user for input
    const readline = await import('readline');
    const rl = readline.createInterface({
      input: process.stdin,
      output: process.stderr,
      terminal: true
    });

    console.error(`🤖 ${ancillaryId} awaiting instructions:`);
    console.error('   (Enter a blank line when done)\n');

    prompt = await new Promise<string>((resolve) => {
      const lines: string[] = [];
      rl.on('line', (line) => {
        if (line === '') {
          // Empty line ends input
          rl.close();
          resolve(lines.join('\n'));
        } else {
          lines.push(line);
        }
      });
      rl.on('close', () => {
        if (lines.length === 0) {
          resolve('');
        }
      });
    });
  }

  if (!prompt) {
    console.error('❌ Error: Prompt cannot be empty');
    process.exit(1);
  }

  console.error('🚀 Toren CLI');
  console.error('═'.repeat(60));
  console.error(`📂 Working directory: ${workingDir}`);
  console.error(`🔗 Ship: ${shipUrl}`);
  console.error(`📝 Prompt length: ${prompt.length} chars`);
  console.error('═'.repeat(60));
  console.error('');

  try {
    // Connect to ship
    console.error('📡 Connecting to ship...');
    const shipClient = new ShipClient(shipUrl, sessionToken, ancillaryId, segmentName);
    await shipClient.connect();
    console.error('✅ Connected\n');

    // Start ancillary runtime
    console.error('🤖 Starting ancillary runtime...');
    const runtime = new AncillaryRuntime(apiKey, shipClient, ancillaryId);
    await runtime.start();
    console.error('✅ Runtime ready\n');

    console.error('═'.repeat(60));
    console.error('🎯 Processing directive...');
    console.error('═'.repeat(60));
    console.error('');

    // Process the directive
    const response = await runtime.processDirective(prompt, undefined, workingDir);

    // Output response to stdout (not stderr)
    console.error('');
    console.error('═'.repeat(60));
    console.error('✨ COMPLETE');
    console.error('═'.repeat(60));
    console.error('');

    // Final response goes to stdout
    console.log(response);

    // Cleanup
    await shipClient.disconnect();
    process.exit(0);

  } catch (error: any) {
    console.error('');
    console.error('═'.repeat(60));
    console.error('❌ ERROR');
    console.error('═'.repeat(60));
    console.error('');
    console.error(error.message || error);
    process.exit(1);
  }
}

main();
