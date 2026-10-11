// SPDX-License-Identifier: BUSL-1.1
//! Example sentences the device compares a text with, in several languages.
//! A text close enough in meaning to one of them belongs to its kind, in any
//! language the model knows. The bench measures them on sentences that are
//! not here.

/// Requests about the whole conversation.
pub const SUMMARY_REQUESTS: &[&str] = &[
    "Resume la conversación",
    "Hazme un resumen de lo que hemos hablado",
    "¿De qué hemos estado hablando?",
    "Recapitula todo lo que me has dicho hasta ahora",
    "¿Qué temas hemos tocado en esta conversación?",
    "Resúmeme todo lo anterior en puntos",
    "Summarize this conversation",
    "Give me a summary of everything we've talked about",
    "Recap what we discussed so far",
    "What topics did we cover in this chat?",
    "List the main points of our conversation",
    "Resuma a nossa conversa até agora",
    "Sobre o que falamos até aqui?",
    "Fais un résumé de notre discussion",
    "De quoi avons-nous parlé jusqu'ici ?",
    "Fasse zusammen, worüber wir gesprochen haben",
    "Worüber haben wir bisher geredet?",
    "Riassumi la nostra conversazione",
    "Di cosa abbiamo parlato finora?",
    "Подведи итог нашего разговора",
    "我们刚才聊了什么？请总结一下",
    "これまでの会話を要約して",
];

/// Topics a person may not want to leave the device unnoticed: health,
/// sexuality, beliefs, money trouble, the law, identity documents, family
/// conflict, and harm to oneself.
pub const SENSITIVE: &[&str] = &[
    // Health, physical and mental.
    "Me han diagnosticado cáncer y empiezo quimioterapia",
    "Tomo antidepresivos desde hace dos años",
    "Tengo ataques de ansiedad casi todos los días",
    "I was diagnosed with bipolar disorder",
    "My doctor found a lump and I'm waiting for the biopsy",
    "I'm HIV positive",
    "Estou fazendo tratamento para depressão",
    "J'ai une maladie chronique dont personne ne sait rien",
    "Ich bin seit Jahren alkoholabhängig",
    "Sono incinta di otto settimane",
    "Creo que tengo una infección de transmisión sexual",
    "I had an abortion last year",
    // Harm to oneself.
    "Ya no quiero seguir viviendo",
    "I've been cutting myself again",
    "Penso em me matar",
    // Sexuality and gender.
    "Soy lesbiana y en mi familia no lo saben",
    "I think I might be transgender",
    "Je suis bisexuel mais je le cache au travail",
    "Mi pareja y yo tenemos problemas sexuales",
    // Beliefs.
    "Dejé la religión de mi familia y no se lo he dicho",
    "I'm an atheist living in a very religious country",
    "Milito en un partido político y temo represalias en el trabajo",
    "I attended the protest against the government last week",
    // Money trouble.
    "Tengo deudas que no puedo pagar y me llaman los acreedores",
    "I'm about to go bankrupt",
    "Perdí todos mis ahorros en apuestas",
    "Ich kann meine Miete nicht mehr bezahlen",
    "Mi sueldo es de 1.200 euros y no llego a fin de mes",
    // The law and immigration.
    "Me detuvieron por conducir borracho",
    "I have a criminal record",
    "Estoy en el país sin papeles",
    "My visa expired and I'm still here",
    "Me están investigando por fraude fiscal",
    // Identity documents, credentials, where one lives.
    "Mi DNI es 12345678Z",
    "Here is my social security number",
    "La contraseña de mi correo es",
    "My bank login and PIN are",
    "Mi dirección es calle Alcalá 45, 2º izquierda",
    "Meu CPF é",
    // Family and relationships.
    "Mi marido me pega cuando bebe",
    "I'm cheating on my wife",
    "Mi hijo consume drogas y no sé qué hacer",
    "Estou me divorciando e brigando pela guarda dos filhos",
    "J'ai été victime d'abus quand j'étais enfant",
];

/// Everyday texts that sound like those topics and are not about the
/// person: a pet's health, code, plants, cars, fiction, general knowledge.
/// A passage must be clearly closer to a sensitive example than to these.
pub const NOT_SENSITIVE: &[&str] = &[
    "Mi perro está enfermo y lo llevo al veterinario",
    "My cat needs medication from the vet",
    "El veterinario dice que mi gato tiene un tumor",
    "My dog is getting old and has arthritis",
    "Tengo que vacunar a mi mascota",
    "Mi planta tiene hongos en las hojas",
    "The server is down and the database is corrupted",
    "Hay un bug en el módulo de pagos",
    "We have a lot of technical debt in this codebase",
    "The protagonist of the novel is diagnosed with cancer",
    "What are the symptoms of the flu?",
    "Explícame cómo funciona la quimioterapia",
    "My car broke down on the highway",
    "Sin embargo, no estoy de acuerdo con el plan",
    "Escribe un cuento para niños",
    "Write a poem about autumn",
];
