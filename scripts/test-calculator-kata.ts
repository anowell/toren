#!/usr/bin/env tsx

/**
 * Calculator Kata - End-to-End Test
 *
 * This test demonstrates the full Toren architecture:
 * 1. Creates a project directory with git init
 * 2. Connects to the Toren daemon
 * 3. Starts an ancillary (Claude agent)
 * 4. Directs Claude to build a CLI calculator
 * 5. Verifies the implementation works
 */

import { AncillaryRuntime } from '../ancillary/dist/ancillary-runtime.js';
import { ShipClient } from '../ancillary/dist/ship-client.js';
import { execSync, spawn } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import * as dotenv from 'dotenv';

dotenv.config();

const PROJECT_DIR = path.join(process.cwd(), 'examples', 'calculator');
const SHIP_URL = process.env.SHIP_URL || 'http://localhost:8787';

let daemonProcess: any = null;

async function cleanup() {
  console.log('🧹 Cleaning up previous implementation...');
  if (fs.existsSync(PROJECT_DIR)) {
    fs.rmSync(PROJECT_DIR, { recursive: true, force: true });
  }
}

async function setup() {
  console.log('📁 Creating project directory...');
  fs.mkdirSync(PROJECT_DIR, { recursive: true });

  console.log('🔧 Initializing git repository...');
  execSync('git init', { cwd: PROJECT_DIR, stdio: 'pipe' });
  execSync('git config user.name "Toren Test"', { cwd: PROJECT_DIR, stdio: 'pipe' });
  execSync('git config user.email "test@toren.dev"', { cwd: PROJECT_DIR, stdio: 'pipe' });
}

async function startDaemon(): Promise<string> {
  console.log('🚢 Starting Toren daemon...');

  return new Promise((resolve, reject) => {
    daemonProcess = spawn('./target/release/toren-daemon', [], {
      cwd: process.cwd(),
      stdio: ['ignore', 'pipe', 'pipe']
    });

    let output = '';
    let pairingToken = '';

    daemonProcess.stdout.on('data', (data: Buffer) => {
      output += data.toString();

      // Look for pairing token in output
      const match = output.match(/Pairing token: (\d{6})/);
      if (match && !pairingToken) {
        pairingToken = match[1];
        console.log(`✅ Daemon started. Pairing token: ${pairingToken}`);

        // Give it a moment to fully initialize
        setTimeout(() => resolve(pairingToken), 500);
      }
    });

    daemonProcess.stderr.on('data', (data: Buffer) => {
      console.error('Daemon stderr:', data.toString());
    });

    daemonProcess.on('error', (error: Error) => {
      reject(error);
    });

    // Timeout after 10 seconds
    setTimeout(() => {
      if (!pairingToken) {
        reject(new Error('Failed to get pairing token from daemon'));
      }
    }, 10000);
  });
}

async function getSessionToken(pairingToken: string): Promise<string> {
  console.log('🔑 Exchanging pairing token for session token...');

  const response = await fetch(`${SHIP_URL}/pair`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ pairing_token: pairingToken })
  });

  if (!response.ok) {
    throw new Error(`Failed to pair: ${response.statusText}`);
  }

  const data = await response.json() as { session_token: string };
  console.log(`✅ Got session token: ${data.session_token.substring(0, 16)}...`);
  return data.session_token;
}

async function stopDaemon() {
  if (daemonProcess) {
    console.log('🛑 Stopping daemon...');
    daemonProcess.kill('SIGTERM');
    daemonProcess = null;
  }
}

async function testCalculator(sessionToken: string) {
  const shipWsUrl = SHIP_URL.replace('http', 'ws');
  const apiKey = process.env.ANTHROPIC_API_KEY;

  if (!apiKey) {
    throw new Error('ANTHROPIC_API_KEY environment variable is required');
  }

  console.log('\n🚀 Starting Toren Calculator Kata\n');
  console.log('═'.repeat(80));

  // Connect to ship with session token
  console.log(`\n📡 Connecting to ship at ${shipWsUrl}...`);
  const shipClient = new ShipClient(shipWsUrl, sessionToken, 'Calculator One');
  await shipClient.connect();
  console.log('✅ Connected to Toren\n');

  // Start ancillary runtime
  console.log('🤖 Initializing Calculator One ancillary...');
  const runtime = new AncillaryRuntime(apiKey, shipClient, 'Calculator One');
  await runtime.start();
  console.log('✅ Calculator One ready\n');

  console.log('═'.repeat(80));
  console.log('📋 DIRECTIVE: Build CLI Calculator');
  console.log('═'.repeat(80));

  const directive = `You are working in the directory: ${PROJECT_DIR}

Build a command-line calculator in Node.js that:

1. Takes a mathematical expression as a command-line argument
2. Supports the four basic operators: + - * /
3. Correctly handles order of operations (multiplication and division before addition and subtraction)
4. Returns the numeric result

Example usage:
  node calculator.js "3 + 2 * 4"  → should output: 11
  node calculator.js "10 / 2 - 3" → should output: 2
  node calculator.js "3 + 2 * 4 / 2" → should output: 7

Requirements:
- Create calculator.js as the main implementation
- Create package.json with appropriate test script
- Write comprehensive tests using a Node.js test framework (like jest or node's built-in test runner)
- Make sure all tests pass
- Commit the code to git with a descriptive commit message

Do NOT just plan - actually write the files, run the tests, and commit the code.`;

  try {
    console.log('\n🎯 Sending directive to Claude...\n');
    const response = await runtime.processDirective(directive, undefined, PROJECT_DIR);

    console.log('\n' + '═'.repeat(80));
    console.log('📝 FINAL RESPONSE');
    console.log('═'.repeat(80));
    console.log(response);
    console.log('═'.repeat(80));

  } catch (error) {
    console.error('\n❌ Error during directive processing:', error);
    throw error;
  } finally {
    await shipClient.disconnect();
  }
}

async function verifyImplementation() {
  console.log('\n' + '═'.repeat(80));
  console.log('🔍 VERIFICATION');
  console.log('═'.repeat(80));

  // Check files exist
  console.log('\n📂 Checking generated files...');
  const expectedFiles = ['calculator.js', 'package.json'];
  for (const file of expectedFiles) {
    const filePath = path.join(PROJECT_DIR, file);
    if (fs.existsSync(filePath)) {
      console.log(`  ✅ ${file} exists`);
    } else {
      throw new Error(`❌ Missing file: ${file}`);
    }
  }

  // Check git commit
  console.log('\n🔍 Checking git history...');
  try {
    const log = execSync('git log --oneline', {
      cwd: PROJECT_DIR,
      encoding: 'utf-8'
    });
    if (log.trim().length > 0) {
      console.log('  ✅ Git commit exists:');
      console.log('  ' + log.trim().split('\n')[0]);
    } else {
      throw new Error('No git commits found');
    }
  } catch (error) {
    console.error('  ❌ Git commit check failed:', error);
    throw error;
  }

  // Install dependencies if needed
  console.log('\n📦 Installing dependencies...');
  try {
    execSync('npm install', { cwd: PROJECT_DIR, stdio: 'pipe' });
    console.log('  ✅ Dependencies installed');
  } catch (error) {
    console.log('  ⚠️  No dependencies or install not needed');
  }

  // Run the generated tests
  console.log('\n🧪 Running generated tests...');
  try {
    const testOutput = execSync('npm test', {
      cwd: PROJECT_DIR,
      encoding: 'utf-8',
      stdio: 'pipe'
    });
    console.log('  ✅ All generated tests passed');
    console.log('  Output:', testOutput.substring(0, 200) + '...');
  } catch (error: any) {
    console.error('  ❌ Generated tests failed:', error.message);
    throw error;
  }

  // Run our own test cases
  console.log('\n🎯 Running verification test cases...');
  const testCases = [
    { expr: '3 + 2 * 4', expected: 11 },
    { expr: '10 / 2 - 3', expected: 2 },
    { expr: '3 + 2 * 4 / 2', expected: 7 },
    { expr: '8 - 2 * 2', expected: 4 },
    { expr: '15 / 3 + 2', expected: 7 }
  ];

  for (const { expr, expected } of testCases) {
    try {
      const result = execSync(`node calculator.js "${expr}"`, {
        cwd: PROJECT_DIR,
        encoding: 'utf-8'
      }).trim();

      const numResult = parseFloat(result);
      if (Math.abs(numResult - expected) < 0.001) {
        console.log(`  ✅ "${expr}" = ${result} (expected: ${expected})`);
      } else {
        throw new Error(`Expected ${expected}, got ${result}`);
      }
    } catch (error: any) {
      console.error(`  ❌ "${expr}": ${error.message}`);
      throw error;
    }
  }

  console.log('\n' + '═'.repeat(80));
  console.log('🎉 ALL VERIFICATIONS PASSED!');
  console.log('═'.repeat(80));
}

async function main() {
  try {
    await cleanup();
    await setup();

    // Start daemon and get auth tokens
    const pairingToken = await startDaemon();
    const sessionToken = await getSessionToken(pairingToken);

    // Run the test
    await testCalculator(sessionToken);
    await verifyImplementation();

    console.log('\n✨ Calculator Kata completed successfully!\n');

    await stopDaemon();
    process.exit(0);
  } catch (error) {
    console.error('\n💥 Test failed:', error);
    await stopDaemon();
    process.exit(1);
  }
}

// Handle cleanup on exit
process.on('SIGINT', async () => {
  await stopDaemon();
  process.exit(1);
});

process.on('SIGTERM', async () => {
  await stopDaemon();
  process.exit(1);
});

main();
