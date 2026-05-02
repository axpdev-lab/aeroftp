# Guida per Connettersi a Google Drive con AeroFTP

Questa guida ti spiega passo dopo passo come configurare AeroFTP per connetterti a Google Drive utilizzando le tue credenziali OAuth personalizzate. La procedura è basata sulla documentazione di rclone, ma adattata per l'interfaccia grafica di AeroFTP.

## Prerequisiti

- Un account Google con accesso a Google Cloud Console
- AeroFTP installato sul tuo sistema
- Un browser web per l'autorizzazione OAuth

## Passo 1: Creazione dell'App Google in Cloud Console

### 1.1 Accedi a Google Cloud Console

1. Vai al sito [Google Cloud Console](https://console.cloud.google.com/)
2. Accedi con il tuo account Google
3. Se non hai ancora un progetto, creane uno nuovo:
   - Clicca su "Seleziona un progetto" in alto a sinistra
   - Clicca su "Nuovo progetto"
   - Dai un nome al progetto (es. "AeroFTP-Drive")
   - Seleziona l'organizzazione se applicabile
   - Clicca su "Crea"

### 1.2 Abilita Google Drive API

1. Nella console, vai alla sezione "API e servizi" → "Libreria"
2. Cerca "Google Drive API"
3. Seleziona "Google Drive API" dai risultati
4. Clicca su "Abilita"

### 1.3 Configura il consenso OAuth

1. Vai su "API e servizi" → "Schermata di consenso OAuth"
2. Scegli il tipo di utente: "Esterno" (per uso personale) o "Interno" (per Google Workspace)
3. Compila le informazioni dell'app:
   - **Nome app**: AeroFTP
   - **Email supporto utente**: il tuo indirizzo email
   - **Domini autorizzati**: lascia vuoto per uso personale
   - **Contatti sviluppatore**: il tuo indirizzo email
4. Nella sezione "Ambiti", aggiungi l'ambito per Google Drive:
   - Clicca su "AGGIUNGI O RIMUOVI AMBITI"
   - Cerca e seleziona ".../auth/drive" (accesso completo) o ".../auth/drive.file" (solo file creati da AeroFTP)
5. Salva e continua

### 1.4 Crea le Credenziali OAuth

1. Vai su "API e servizi" → "Credenziali"
2. Clicca su "Crea credenziali" → "ID client OAuth"
3. Seleziona "Applicazione web" come tipo
4. Nella sezione "URI di reindirizzamento autorizzati", aggiungi:
   - `http://127.0.0.1` (AeroFTP gestisce automaticamente la porta)
5. Clicca su "Crea"
6. **IMPORTANTE**: Copia e salva il **Client ID** e il **Client Secret** che vengono mostrati. Li userai in AeroFTP.

## Passo 2: Configurazione di AeroFTP

### 2.1 Apri le Impostazioni OAuth

1. Avvia AeroFTP
2. Vai nelle impostazioni dell'applicazione (generalmente tramite menu o icona ingranaggio)
3. Cerca la sezione "OAuth" o "Provider"
4. Seleziona "Google API" o "Google Drive"

### 2.2 Inserisci le Credenziali

1. Nel campo "Client ID", incolla il Client ID ottenuto da Google Cloud Console
2. Nel campo "Client Secret", incolla il Client Secret
3. Alcuni campi potrebbero avere placeholder come:
   - Client ID: `xxxxxxxx.apps.googleusercontent.com`
   - Client Secret: `GOCSPX-...`
4. Verifica che i valori siano corretti
5. Salva le impostazioni

### 2.3 Autorizzazione Iniziale

1. La prima volta che usi Google Drive con AeroFTP, l'applicazione aprirà automaticamente il tuo browser web
2. Accedi con il tuo account Google se necessario
3. Nella schermata di consenso, clicca su "Consenti" per autorizzare AeroFTP ad accedere a Google Drive
4. Il browser potrebbe mostrare un messaggio di conferma o reindirizzare a una pagina locale

## Passo 3: Test della Connessione

### 3.1 Verifica la Connessione

Usa i comandi AeroFTP per testare la connessione:

```bash
# Lista i profili salvati
aeroftp-cli profiles --json

# Test connessione a Google Drive (sostituisci "MyDrive" con il nome del tuo profilo)
aeroftp-cli connect --profile "MyDrive"

# Lista file nella root di Google Drive
aeroftp-cli ls --profile "MyDrive" /

# Ottieni informazioni sul profilo e quota
aeroftp-cli about --profile "MyDrive" --json
```

### 3.2 Esempi di Operazioni Comuni

```bash
# Scarica un file
aeroftp-cli get --profile "MyDrive" /percorso/remoto/file.txt ./file_locale.txt

# Carica un file
aeroftp-cli put --profile "MyDrive" ./file_locale.txt /percorso/remoto/file.txt

# Sincronizza una cartella
aeroftp-cli sync --profile "MyDrive" ./cartella_locale /cartella_remota --dry-run

# Crea una directory
aeroftp-cli mkdir --profile "MyDrive" /nuova_cartella
```

## Sicurezza e Best Practices

### Gestione delle Credenziali
- AeroFTP memorizza le credenziali in un vault criptato (AES-256-GCM)
- Le credenziali non sono mai esposte nei comandi CLI o nei log
- Usa sempre profili nominati invece di inserire credenziali manualmente

### Ambiti di Accesso
- Scegli l'ambito appropriato in base alle tue esigenze:
  - `drive`: Accesso completo a tutti i file
  - `drive.file`: Solo file creati da AeroFTP
  - `drive.readonly`: Solo lettura

### Condivisione e Sicurezza
- Le credenziali sono isolate per profilo
- Puoi avere multiple configurazioni Google Drive con account diversi
- Le autorizzazioni possono essere revocate dalla [Google Account settings](https://myaccount.google.com/permissions)

## Troubleshooting

### Errore: "invalid_client"
- Verifica che Client ID e Client Secret siano corretti
- Assicurati che l'app OAuth sia configurata per "Applicazione web"

### Errore: "redirect_uri_mismatch"
- Verifica che `http://127.0.0.1` sia negli URI di reindirizzamento autorizzati
- AeroFTP gestisce automaticamente la porta dinamica

### Errore: "access_denied"
- Verifica che tu abbia cliccato "Consenti" nella schermata di autorizzazione
- Controlla che l'account Google abbia accesso a Google Drive

### Connessione lenta o timeout
- Verifica la connessione internet
- Alcuni provider potrebbero avere limiti di velocità
- Usa `--fast-list` per elenchi più veloci (se supportato)

### Problemi con file grandi
- Google Drive ha limiti di caricamento (750GB/giorno)
- Usa `--chunk-size` per ottimizzare i caricamenti

## Differenze con rclone

Mentre rclone richiede configurazione manuale via CLI, AeroFTP offre:

- **Interfaccia grafica**: Configurazione tramite GUI intuitiva
- **Sicurezza migliorata**: Vault criptato invece di file di configurazione
- **Automazione**: Autorizzazione OAuth automatica senza intervento manuale
- **Integrazione**: Supporto nativo per molteplici provider cloud

## Risorse Aggiuntive

- [Documentazione ufficiale Google Drive API](https://developers.google.com/drive/api/v3/quickstart)
- [Guida rclone per Google Drive](https://rclone.org/drive/)
- [Documentazione AeroFTP](https://docs.aeroftp.app/)

---

*Questa guida è stata creata per AeroFTP v3.7.0. Le procedure potrebbero variare leggermente con versioni future.*