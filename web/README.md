# Toren Web Interface

Mobile-first web interface for controlling Toren (distributed development intelligence).

## Features

- **Mobile-First Design**: Responsive layout optimized for phones, tablets, and desktops
- **WebSocket Connection**: Real-time bidirectional communication with Toren daemon
- **Pairing Flow**: Secure token-based authentication
- **Chat Interface**: Intuitive conversation-based control
- **Session Persistence**: Auto-reconnect with stored credentials
- **Command Output Streaming**: Real-time display of command execution

## Tech Stack

- **SvelteKit 2.x** - Static SPA with adapter-static
- **TypeScript** - Type-safe development
- **Biome** - Fast linting and formatting
- **Vitest** - Unit testing
- **Testing Library** - Component testing

## Development

```bash
# Install dependencies
pnpm install

# Start dev server (port 5174)
pnpm dev

# Run tests
pnpm test

# Watch mode
pnpm test:watch

# Lint
pnpm lint

# Format
pnpm format

# Build for production
pnpm build

# Preview production build
pnpm preview
```

## Project Structure

```
web/
├── src/
│   ├── lib/
│   │   ├── components/          # Svelte components
│   │   │   ├── ChatInterface.svelte
│   │   │   └── PairingModal.svelte
│   │   ├── stores/              # State management
│   │   │   └── toren.ts         # WebSocket client & store
│   │   └── types/               # TypeScript types
│   │       └── toren.ts
│   ├── routes/                  # SvelteKit routes
│   │   ├── +layout.svelte
│   │   ├── +layout.ts
│   │   └── +page.svelte
│   ├── test/                    # Test setup
│   │   └── setup.ts
│   └── app.css                  # Global styles
├── static/                      # Static assets
├── biome.json                   # Biome config
├── svelte.config.js             # SvelteKit config
├── vite.config.ts               # Vite config
├── vitest.config.ts             # Vitest config
└── package.json
```

## Architecture

### WebSocket Protocol

The web interface communicates with the Toren daemon via WebSocket at `ws://localhost:8787/ws`.

**Request Types:**
- `Auth` - Authenticate with session token
- `Command` - Execute commands
- `FileRead` - Read file contents
- `VcsStatus` - Get VCS status

**Response Types:**
- `AuthSuccess` / `AuthFailure`
- `CommandOutput` - Streaming command output
- `FileContent` - File contents
- `VcsStatus` - VCS status information
- `Error` - Error messages

### State Management

Uses Svelte stores for reactive state management:

- `torenStore` - Main application state
- `isConnected` - Derived connection status
- `isAuthenticated` - Derived auth status
- `messages` - Derived message list

### Testing Strategy

**High-Value Tests:**
- Store initialization and state management
- Connection state transitions
- Authentication flow
- Message handling

**Not Tested (Low Value):**
- UI component rendering details
- Styling and layout
- Simple prop passing

## Usage

1. Start the Toren daemon:
   ```bash
   just daemon
   ```

2. Note the pairing token from the daemon output

3. Start the web interface:
   ```bash
   cd web && pnpm dev
   ```

4. Open http://localhost:5174 in your browser

5. Enter the pairing token and connect

6. Start chatting with your development environment!

## Future Enhancements

- [ ] Ancillary selection and management
- [ ] File browser
- [ ] VCS integration UI
- [ ] Command palette
- [ ] Diff viewer
- [ ] Multi-segment support
- [ ] Offline mode
- [ ] Push notifications
- [ ] PWA support

## License

MIT
