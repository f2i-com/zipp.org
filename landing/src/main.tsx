import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import App from './App'
import './styles.css'
import './experience.css'
import './demos.css'

// The static introduction in index.html exists for readers and crawlers that
// never run this script; the app renders the same links itself.
document.getElementById('static-intro')?.remove()

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
