import { render } from 'preact'
import { App } from './app'
import { connect } from './store'
import './styles.css'

connect()
render(<App />, document.getElementById('app')!)
